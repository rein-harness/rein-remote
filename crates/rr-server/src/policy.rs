//! Who may do what (`--policy FILE`, TOML or JSON): rules matched in order against the viewer (their
//! user name and groups from the identity provider, or how they got in: a token, a paired device, the
//! relay), the first that matches decides. No rule matching: refused.
//!
//! ```toml
//! [[rule]]
//! name = "admins"
//! groups = ["rr-admins"]            # any of these groups (or users = [...], via = [...])
//! record = true
//!
//! [[rule]]
//! name = "support, view only"
//! groups = ["support"]
//! view_only = true                  # input dropped by the server
//! clipboard = false                 # pastes and clipboard messages dropped
//! audio = false                     # no sound
//! sources = ["window:*"]            # only when this server shows a window
//! max_session_minutes = 30
//!
//! [[rule]]
//! name = "contractors"
//! users = ["*@contractor.example"]
//! allow = false                     # refused, with this rule's name as the reason
//! ```
//!
//! Matching: `users` against the user name (case-insensitive; `*` for anyone, `*@domain` for a
//! domain), `groups` against any of the viewer's groups (exactly), `via` against how they came in
//! (`oidc`, `saml`, `token`, `paired`, `relay`, `open`). A rule with several of these needs all of them;
//! one with none matches everyone. Without `--policy`, every viewer who gets past the gate has full
//! access (as before policies).

use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub users: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub via: Vec<String>,
    #[serde(default = "yes")]
    pub allow: bool,
    #[serde(default)]
    pub view_only: bool,
    #[serde(default = "yes")]
    pub clipboard: bool,
    #[serde(default = "yes")]
    pub audio: bool,
    /// Record this viewer's sessions (when the server records: `--record DIR`). Default: whatever
    /// `--record` says (on when given).
    #[serde(default)]
    pub record: Option<bool>,
    /// The sources this rule allows (`*`, `display`, `display:*`, `window:Simulator`, `window:*`,
    /// `app:*`, `vnc:*`, `pipe`, `kms`, `test-pattern`). Default: any.
    #[serde(default)]
    pub sources: Vec<String>,
    /// How long a session may last from the sign-in, in minutes (fractions too); 0 or absent: no limit.
    #[serde(default)]
    pub max_session_minutes: f64,
}

fn yes() -> bool {
    true
}

/// What a viewer may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Perms {
    pub input: bool,
    pub clipboard: bool,
    pub audio: bool,
    /// None: as `--record` says.
    pub record: Option<bool>,
    pub max_session: Option<Duration>,
}

impl Perms {
    pub const FULL: Perms = Perms {
        input: true,
        clipboard: true,
        audio: true,
        record: None,
        max_session: None,
    };
}

/// A policy's answer for one viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Let in, with these permissions, by this rule (its name, or `rule N`; `none` without a policy).
    Allow {
        rule: String,
        perms: Perms,
    },
    Deny {
        rule: Option<String>,
        reason: String,
    },
}

/// The viewer, as the policy sees them.
pub struct Who<'a> {
    pub user: &'a str,
    pub groups: &'a [String],
    pub via: &'a str,
}

/// `*` anything; `*x` ends with x; `x*` starts with x; else equal. Case-insensitive when `fold`.
fn glob(pat: &str, s: &str, fold: bool) -> bool {
    let (p, s) = if fold {
        (pat.to_lowercase(), s.to_lowercase())
    } else {
        (pat.to_string(), s.to_string())
    };
    if p == "*" {
        true
    } else if let Some(end) = p.strip_prefix('*') {
        s.ends_with(end)
    } else if let Some(start) = p.strip_suffix('*') {
        s.starts_with(start)
    } else {
        p == s
    }
}

impl Policy {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("--policy {}: {e}", path.display()))?;
        let p: Policy = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
            serde_json::from_str(&text).map_err(|e| format!("--policy {}: {e}", path.display()))?
        } else {
            toml::from_str(&text).map_err(|e| format!("--policy {}: {e}", path.display()))?
        };
        if p.rules.is_empty() {
            return Err(format!("--policy {}: no [[rule]]s, so it would refuse everyone", path.display()));
        }
        Ok(p)
    }

    /// The first rule that matches `who` decides, for this server's `source`.
    pub fn decide(&self, who: &Who, source: &str) -> Decision {
        for (i, r) in self.rules.iter().enumerate() {
            let matches = (r.users.is_empty() || r.users.iter().any(|u| glob(u, who.user, true)))
                && (r.groups.is_empty() || r.groups.iter().any(|g| who.groups.iter().any(|have| glob(g, have, false))))
                && (r.via.is_empty() || r.via.iter().any(|v| v.eq_ignore_ascii_case(who.via)));
            if !matches {
                continue;
            }
            let name = r.name.clone().unwrap_or_else(|| format!("rule {}", i + 1));
            if !r.allow {
                return Decision::Deny {
                    reason: format!("the policy refuses {} (\"{name}\")", who.user),
                    rule: Some(name),
                };
            }
            if !r.sources.is_empty() && !r.sources.iter().any(|s| glob(s, source, false)) {
                return Decision::Deny {
                    reason: format!("{} may not view this machine's {source} (\"{name}\")", who.user),
                    rule: Some(name),
                };
            }
            return Decision::Allow {
                rule: name,
                perms: Perms {
                    input: !r.view_only,
                    clipboard: r.clipboard,
                    audio: r.audio,
                    record: r.record,
                    max_session: (r.max_session_minutes > 0.0).then(|| Duration::from_secs_f64(r.max_session_minutes * 60.0)),
                },
            };
        }
        Decision::Deny {
            rule: None,
            reason: format!("no rule in the policy lets {} in", who.user),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[[rule]]
name = "admins"
groups = ["rr-admins"]
record = true

[[rule]]
name = "support"
groups = ["support"]
view_only = true
clipboard = false
audio = false
sources = ["window:*", "test-pattern"]
max_session_minutes = 30

[[rule]]
name = "contractors"
users = ["*@contractor.example"]
allow = false

[[rule]]
name = "the link"
via = ["token"]
"#;

    fn who<'a>(user: &'a str, groups: &'a [String], via: &'a str) -> Who<'a> {
        Who { user, groups, via }
    }

    #[test]
    fn the_first_matching_rule_decides() {
        let p: Policy = toml::from_str(EXAMPLE).unwrap();
        let admins = vec!["rr-admins".to_string()];
        let support = vec!["support".to_string()];
        let none: Vec<String> = vec![];
        match p.decide(&who("alice@corp.example", &admins, "oidc"), "display") {
            Decision::Allow { rule, perms } => {
                assert_eq!(rule, "admins");
                assert_eq!(perms.record, Some(true));
                assert!(perms.input && perms.clipboard && perms.audio && perms.max_session.is_none());
            }
            d => panic!("{d:?}"),
        }
        match p.decide(&who("bob@corp.example", &support, "saml"), "test-pattern") {
            Decision::Allow { rule, perms } => {
                assert_eq!(rule, "support");
                assert_eq!(
                    perms,
                    Perms {
                        input: false,
                        clipboard: false,
                        audio: false,
                        record: None,
                        max_session: Some(Duration::from_secs(1800)),
                    }
                );
            }
            d => panic!("{d:?}"),
        }
        // Support may view windows, not the whole display.
        assert!(matches!(
            p.decide(&who("bob@corp.example", &support, "oidc"), "display"),
            Decision::Deny { rule: Some(r), .. } if r == "support"
        ));
        assert!(matches!(p.decide(&who("bob", &support, "oidc"), "window:Simulator"), Decision::Allow { .. }));
        // A contractor (case doesn't matter in user names), even one in a group further down.
        assert!(matches!(
            p.decide(&who("Eve@Contractor.Example", &none, "oidc"), "display"),
            Decision::Deny { rule: Some(r), .. } if r == "contractors"
        ));
        // The link's token, by how it came in.
        assert!(matches!(p.decide(&who("token", &none, "token"), "display"), Decision::Allow { rule, .. } if rule == "the link"));
        // Someone no rule names.
        let d = p.decide(&who("carol@corp.example", &none, "oidc"), "display");
        assert_eq!(
            d,
            Decision::Deny {
                rule: None,
                reason: "no rule in the policy lets carol@corp.example in".into()
            }
        );
        // Groups match exactly (no case folding: IdPs' group names are identifiers).
        assert!(matches!(
            p.decide(&who("x", &["RR-ADMINS".to_string()], "oidc"), "display"),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn json_works_too_and_typos_are_caught() {
        let dir = std::env::temp_dir().join(format!("rr-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("p.json");
        std::fs::write(&json, r#"{"rule":[{"users":["*"],"view_only":true}]}"#).unwrap();
        let p = Policy::load(&json).unwrap();
        assert!(matches!(p.decide(&who("x", &[], "oidc"), "display"), Decision::Allow { perms, .. } if !perms.input));
        let typo = dir.join("p.toml");
        std::fs::write(&typo, "[[rule]]\nview_ony = true\n").unwrap();
        assert!(Policy::load(&typo).unwrap_err().contains("view_ony"));
        std::fs::write(&typo, "").unwrap();
        assert!(Policy::load(&typo).unwrap_err().contains("no [[rule]]s"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
