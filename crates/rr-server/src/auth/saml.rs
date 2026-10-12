//! SAML 2.0 sign-in (`--features saml`), SP-initiated: the AuthnRequest goes to the IdP by the
//! HTTP-Redirect binding, its Response comes back by HTTP-POST to `/sso/saml/acs`.
//!
//! The response is checked by samael on top of xmlsec1 (XML canonicalization and the signature's
//! references done by the library every SAML deployment leans on, not by hand): the signature against
//! the IdP's certificates from its metadata (SHA-256 or stronger; only the signed part of the document
//! is read afterwards, so nothing can be wrapped around it), the destination (this ACS), the issuer,
//! the times (with three minutes' leeway), `InResponseTo` (a request this server sent, each answered
//! once), and the audience. Here, on top: the IdP's metadata must have a signing certificate (without
//! one samael would read the response unchecked), and the assertion must name this service provider
//! as its audience (samael lets an assertion with no audience restriction through).

use super::{Principal, Resp};
use samael::metadata::{EntityDescriptor, HTTP_REDIRECT_BINDING};
use samael::schema::Assertion;
use samael::service_provider::ServiceProvider;
use samael::traits::ToXml;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const LOGIN_LIFE: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 1000;

pub struct Saml {
    sp: ServiceProvider,
    idp_sso: String,
    entity: String,
    user_attr: Option<String>,
    groups_attr: String,
    /// Requests sent and not yet answered: their ids, where to go after.
    pending: Mutex<HashMap<String, (String, Instant)>>,
}

impl Saml {
    /// `metadata`: the IdP's metadata, a file or an https:// URL.
    pub fn new(metadata: &str, entity: &str, public_url: &str, user_attr: Option<String>, groups_attr: String) -> Result<Self, String> {
        let xml = if metadata.starts_with("https://") || metadata.starts_with("http://") {
            let http = super::oidc::http();
            let mut r = http.get(metadata).call().map_err(|e| format!("{metadata}: {e}"))?;
            if r.status().as_u16() != 200 {
                return Err(format!("{metadata}: HTTP {}", r.status()));
            }
            r.body_mut().read_to_string().map_err(|e| format!("{metadata}: {e}"))?
        } else {
            std::fs::read_to_string(metadata).map_err(|e| format!("{metadata}: {e}"))?
        };
        let idp: EntityDescriptor = xml.parse().map_err(|e| format!("the IdP's metadata: {e}"))?;
        use samael::crypto::AllowedSignatureAlgorithm as A;
        let sp = ServiceProvider {
            entity_id: Some(entity.to_string()),
            metadata_url: Some(format!("{public_url}/sso/saml/metadata")),
            acs_url: Some(format!("{public_url}/sso/saml/acs")),
            slo_url: Some(format!("{public_url}/sso/logout")),
            idp_metadata: idp,
            allow_idp_initiated: false,
            allowed_signature_algorithms: Some(vec![A::RsaSha256, A::RsaSha384, A::RsaSha512, A::EcdsaSha256, A::EcdsaSha384, A::EcdsaSha512]),
            ..ServiceProvider::default()
        };
        // Without a signing certificate samael would take an unsigned response as it is.
        match sp.idp_signing_certs() {
            Ok(Some(c)) if !c.is_empty() => {}
            _ => return Err("the IdP's metadata has no signing certificate".into()),
        }
        let idp_sso = sp
            .sso_binding_location(HTTP_REDIRECT_BINDING)
            .ok_or("the IdP's metadata has no SingleSignOnService for the HTTP-Redirect binding")?;
        Ok(Self {
            sp,
            idp_sso,
            entity: entity.to_string(),
            user_attr,
            groups_attr,
            pending: Mutex::default(),
        })
    }

    pub fn idp_entity(&self) -> String {
        self.sp.idp_metadata.entity_id.clone().unwrap_or_default()
    }

    pub fn metadata(&self) -> String {
        self.sp.metadata().ok().and_then(|m| m.to_string().ok()).unwrap_or_default()
    }

    /// Off to the IdP with an AuthnRequest (HTTP-Redirect binding).
    pub fn login(&self, next: &str) -> Result<Resp, String> {
        let req = self.sp.make_authentication_request(&self.idp_sso).map_err(|e| e.to_string())?;
        let url = req.redirect("").map_err(|e| e.to_string())?.ok_or("no IdP address")?;
        let mut p = self.pending.lock().unwrap();
        p.retain(|_, (_, at)| at.elapsed() < LOGIN_LIFE);
        if p.len() >= MAX_PENDING {
            return Err("too many sign-ins at once".into());
        }
        p.insert(req.id.clone(), (super::local_path(next), Instant::now()));
        Ok(Resp::redirect(url.as_str()))
    }

    /// The IdP's response (the ACS form): who signed in, and where to go next.
    pub fn acs(&self, form: &HashMap<String, String>) -> Result<(Principal, String), String> {
        let response = form.get("SAMLResponse").ok_or("no SAMLResponse")?;
        let ids: Vec<String> = {
            let mut p = self.pending.lock().unwrap();
            p.retain(|_, (_, at)| at.elapsed() < LOGIN_LIFE);
            p.keys().cloned().collect()
        };
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        // Base64 with line breaks, as some IdPs send it.
        let response: String = response.split_whitespace().collect();
        let a = self
            .sp
            .parse_base64_response(&response, Some(&refs))
            .map_err(|e| format!("the SAML response didn't check out: {e}"))?;
        self.audience(&a)?;
        // The request it answers is spent (a response can't be played again).
        let answered = in_response_to(&a).ok_or("the assertion answers no request")?;
        let (next, _) = self.pending.lock().unwrap().remove(&answered).ok_or("that sign-in was already used")?;
        let (user, groups) = self.who(&a)?;
        Ok((
            Principal {
                user,
                groups,
                via: "saml",
                since: SystemTime::now(),
            },
            next,
        ))
    }

    fn audience(&self, a: &Assertion) -> Result<(), String> {
        let ok = a
            .conditions
            .as_ref()
            .and_then(|c| c.audience_restrictions.as_ref())
            .is_some_and(|rs| rs.iter().any(|r| r.audience.iter().any(|x| x == &self.entity)));
        if ok {
            Ok(())
        } else {
            Err(format!("the assertion isn't for this service provider ({})", self.entity))
        }
    }

    fn who(&self, a: &Assertion) -> Result<(String, Vec<String>), String> {
        let attrs: Vec<&samael::attribute::Attribute> = a.attribute_statements.iter().flatten().flat_map(|s| s.attributes.iter()).collect();
        let named = |n: &str| -> Vec<String> {
            attrs
                .iter()
                .filter(|x| x.name.as_deref() == Some(n) || x.friendly_name.as_deref() == Some(n))
                .flat_map(|x| x.values.iter().filter_map(|v| v.value.clone()))
                .collect()
        };
        let user = match &self.user_attr {
            Some(attr) => named(attr).into_iter().next(),
            None => a.subject.as_ref().and_then(|s| s.name_id.as_ref()).map(|n| n.value.trim().to_string()),
        }
        .filter(|u| !u.is_empty())
        .ok_or("the assertion names no user")?;
        Ok((user, named(&self.groups_attr)))
    }
}

fn in_response_to(a: &Assertion) -> Option<String> {
    a.subject
        .as_ref()?
        .subject_confirmations
        .as_ref()?
        .iter()
        .find_map(|c| c.subject_confirmation_data.as_ref()?.in_response_to.clone())
}
