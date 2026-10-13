using System.Text.Json.Nodes;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>OIDC or SAML sign-in (<c>/control/admin/signin</c>); the client secret is write-only.</summary>
public sealed partial class SignInPage : Page
{
    HostModel M => App.Model;
    JsonObject loaded = new();

    static readonly string[] OidcKeys = { "oidc-issuer", "oidc-client-id", "oidc-scopes", "oidc-user-claim", "oidc-groups-claim" };
    static readonly string[] SamlKeys = { "saml-idp-metadata", "saml-entity-id", "saml-user-attr", "saml-groups-attr" };

    public SignInPage()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            M.Connected += Load;
            Load();
        };
        Unloaded += (_, _) => M.Connected -= Load;
    }

    TextBox Box(string key) => key switch
    {
        "public-url" => PublicUrl,
        "oidc-issuer" => OidcIssuer,
        "oidc-client-id" => OidcClientId,
        "oidc-scopes" => OidcScopes,
        "oidc-user-claim" => OidcUserClaim,
        "oidc-groups-claim" => OidcGroupsClaim,
        "saml-idp-metadata" => SamlMetadata,
        "saml-entity-id" => SamlEntityId,
        "saml-user-attr" => SamlUserAttr,
        "saml-groups-attr" => SamlGroupsAttr,
        _ => throw new ArgumentException(key),
    };

    async void Load()
    {
        var (s, error) = await M.Call(c => c.GetSignIn());
        if (s is null)
        {
            Ui.Result(SignInResult, error);
            return;
        }
        Fill(s);
    }

    void Fill(JsonObject s)
    {
        loaded = s;
        var kind = SettingsEdit.Text(s["kind"]);
        KindOff.IsChecked = kind is not ("oidc" or "saml");
        KindOidc.IsChecked = kind == "oidc";
        KindSaml.IsChecked = kind == "saml";
        foreach (var k in OidcKeys.Concat(SamlKeys).Append("public-url"))
            Box(k).Text = SettingsEdit.Text(s[k]);
        OidcSecret.Password = "";
        OidcSecretState.Text = SettingsEdit.Flag(s["oidc-client-secret-set"]) ? "A secret is set." : "No secret set.";
        var saml = SettingsEdit.Flag(s["saml_available"]);
        KindSaml.IsEnabled = saml;
        SamlNote.Visibility = saml ? Visibility.Collapsed : Visibility.Visible;
        ShowKind();
    }

    void Kind_Checked(object sender, RoutedEventArgs e) => ShowKind();

    void ShowKind()
    {
        OidcPanel.Visibility = KindOidc.IsChecked == true ? Visibility.Visible : Visibility.Collapsed;
        SamlPanel.Visibility = KindSaml.IsChecked == true ? Visibility.Visible : Visibility.Collapsed;
        PublicUrl.Visibility = KindOff.IsChecked == true ? Visibility.Collapsed : Visibility.Visible;
    }

    void Reload_Click(object sender, RoutedEventArgs e) => Load();

    async void Save_Click(object sender, RoutedEventArgs e)
    {
        var edited = new JsonObject();
        JsonNode? Val(string k) => string.IsNullOrWhiteSpace(Box(k).Text) ? null : JsonValue.Create(Box(k).Text.Trim());
        if (KindOff.IsChecked == true)
        {
            // Off: as the doc says, no issuer and no metadata.
            edited["oidc-issuer"] = null;
            edited["saml-idp-metadata"] = null;
        }
        else
        {
            edited["public-url"] = Val("public-url");
            var (mine, other) = KindOidc.IsChecked == true ? (OidcKeys, SamlKeys) : (SamlKeys, OidcKeys);
            foreach (var k in mine)
                edited[k] = Val(k);
            // The other kind is turned off.
            edited[other[0]] = null;
        }
        var changes = SettingsEdit.Diff(loaded, edited);
        if (KindOidc.IsChecked == true && OidcSecret.Password.Length > 0)
            changes["oidc-client-secret"] = OidcSecret.Password;
        if (changes.Count == 0)
        {
            Ui.Result(SignInResult, null, "Nothing changed.");
            return;
        }
        SaveSignIn.IsEnabled = false;
        var (r, error) = await M.Call(c => c.PutSignIn(changes));
        SaveSignIn.IsEnabled = true;
        if (r is null)
        {
            Ui.Result(SignInResult, error);
            return;
        }
        Fill(r);
        Ui.Result(SignInResult, null, SettingsEdit.Applied(r["applied"] is JsonValue a ? SettingsEdit.Text(a) : null));
    }
}
