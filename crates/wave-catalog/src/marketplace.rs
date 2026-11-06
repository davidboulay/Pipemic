//! Wave Link add-ons on the Elgato marketplace (public catalogue API, no
//! login). Only lists products; downloads need the user's Elgato account
//! and happen on the website. Blocking; call from a worker thread in GUIs.

pub const API: &str = "https://mp-gateway.elgato.com";
pub const SITE: &str = "https://marketplace.elgato.com";

#[derive(Debug, Clone, PartialEq)]
pub struct Addon {
    pub id: String,
    pub name: String,
    /// Publisher, e.g. "Elgato".
    pub org: String,
    /// "presets" (EQ preset XML) or "vst" (plugin).
    pub extension: String,
    /// First paragraph of the description, plain text.
    pub summary: String,
    /// Product page, where the user downloads it.
    pub url: String,
    /// Variant names (e.g. "default", or "Windows" + "macOS").
    pub variants: Vec<String>,
    /// Variant ids (what `wavelink://presets/install/<id>` links carry).
    pub variant_ids: Vec<String>,
    /// Thumbnail image (PNG on the CDN).
    pub thumbnail: Option<String>,
    /// Latest version, e.g. "1.0.0" (from the product details; only fetched
    /// for add-ons that run on Linux).
    pub version: Option<String>,
}

impl Addon {
    /// Can Pipemic use it natively? EQ presets: yes. Plugins need a native
    /// Linux build, which no listed plugin is known to ship yet.
    pub fn runs_on_linux(&self) -> bool {
        self.extension == "presets"
    }
}

/// Every Wave Link add-on in the catalogue.
pub fn wave_link_addons() -> Result<Vec<Addon>, String> {
    let mut out = Vec::new();
    let mut url = format!("{API}/products?application=wave-link&limit=50&offset=0");
    for _ in 0..20 {
        let body = ureq::get(&url)
            .call()
            .map_err(|e| format!("marketplace: {e}"))?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("marketplace: {e}"))?;
        let (page, next) = parse_page(&body)?;
        out.extend(page);
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    // Versions, fetched in parallel. Best effort: an add-on without a
    // version just can't show updates.
    std::thread::scope(|sc| {
        for a in out.iter_mut().filter(|a| a.runs_on_linux()) {
            sc.spawn(move || a.version = get_string(&format!("{API}/products/{}", a.id)).ok().and_then(|b| latest_version(&b)));
        }
    });
    Ok(out)
}

fn get_string(url: &str) -> Result<String, String> {
    ureq::get(url).call().map_err(|e| format!("marketplace: {e}"))?.body_mut().read_to_string().map_err(|e| format!("marketplace: {e}"))
}

/// Raw bytes of a public marketplace file (thumbnails), at most 4 MB.
pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://mp-cdn.elgato.com/") {
        return Err(format!("not a marketplace file: {url}"));
    }
    ureq::get(url)
        .call()
        .map_err(|e| format!("marketplace: {e}"))?
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("marketplace: {e}"))
}

/// A signed-in marketplace website session: its cookies, as the sign-in
/// window (`pipemic-login`) saved them.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub cookies: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstallError {
    /// No session, or the website no longer accepts it.
    SignIn,
    /// The website wouldn't hand out the file (e.g. not in the library yet).
    NotOwned,
    Other(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignIn => f.write_str("sign in to Elgato first"),
            Self::NotOwned => f.write_str("get it on the marketplace first"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

/// An add-on file fetched through the user's session.
#[derive(Debug, Clone)]
pub struct Download {
    pub product_name: String,
    /// e.g. "1" or "1.0.0".
    pub version: String,
    /// File name from the download link, e.g. "Old_Radio-1.0.0.xml".
    pub file_name: String,
    pub bytes: Vec<u8>,
}

impl Session {
    fn header(&self) -> String {
        self.cookies.iter().filter(|(n, _)| !n.starts_with("__stripe")).map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; ")
    }

    /// Take cookies the website renewed (Set-Cookie) into the session.
    fn update(&mut self, set_cookies: &[String]) -> bool {
        let mut changed = false;
        for sc in set_cookies {
            let Some((name, rest)) = sc.split_once('=') else { continue };
            let value = rest.split(';').next().unwrap_or("").to_string();
            let name = name.trim().to_string();
            match self.cookies.iter_mut().find(|(n, _)| *n == name) {
                Some((_, v)) if *v != value => {
                    *v = value;
                    changed = true;
                }
                None => {
                    self.cookies.push((name, value));
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }
}

/// Fetch add-on variant `variant` the way the website's Download button
/// does: ask `purchase-link` with the session, then download the signed
/// CDN link. Returns the file and whether the session's cookies changed.
pub fn download_variant(session: &mut Session, variant: &str) -> Result<(Download, bool), InstallError> {
    if session.cookies.is_empty() {
        return Err(InstallError::SignIn);
    }
    if variant.is_empty() || !variant.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(InstallError::Other(format!("bad add-on id {variant:?}")));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut resp = agent
        .get(&format!("{SITE}/api/proxy/purchase-link/{variant}"))
        .header("Cookie", &session.header())
        .header("Accept", "application/json")
        .call()
        .map_err(|e| InstallError::Other(format!("marketplace: {e}")))?;
    let set: Vec<String> = resp.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok().map(String::from)).collect();
    let changed = session.update(&set);
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    match status {
        200 => {}
        401 | 403 => return Err(InstallError::SignIn),
        404 | 400 | 402 => return Err(InstallError::NotOwned),
        s => return Err(InstallError::Other(format!("marketplace answered {s}"))),
    }
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| InstallError::Other("marketplace reply is not JSON".into()))?;
    let link = v["direct_link"].as_str().ok_or(InstallError::NotOwned)?;
    if !link.starts_with("https://mp-cdn.elgato.com/") {
        return Err(InstallError::Other("unexpected download host".into()));
    }
    let file_name = percent_decode(link.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("addon"));
    let bytes = ureq::get(link)
        .call()
        .map_err(|e| InstallError::Other(format!("download: {e}")))?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| InstallError::Other(format!("download: {e}")))?;
    let version = match &v["version_number"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    Ok((Download { product_name: v["product_name"].as_str().unwrap_or("").to_string(), version, file_name, bytes }, changed))
}

/// "Retro's%20Cinema%20EQ-1.xml" -> "Retro's Cinema EQ-1.xml".
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Name and thumbnail of an add-on variant (public, no sign-in).
pub fn variant_info(variant: &str) -> Result<(String, Option<String>), String> {
    let body = get_string(&format!("{API}/allitems?ids={variant}"))?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| "marketplace reply is not JSON".to_string())?;
    let it = v.as_array().and_then(|a| a.first()).ok_or("unknown add-on")?;
    Ok((
        it["product_name"].as_str().or(it["name"].as_str()).unwrap_or("").to_string(),
        it["thumbnail_cdn"].as_str().map(String::from),
    ))
}

/// `latest_version_number` of a product's default (else first) variant.
pub fn latest_version(details: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(details).ok()?;
    let vars = v["variants"].as_array()?;
    let pick = vars.iter().find(|x| x["is_default"] == true).or(vars.first())?;
    pick["latest_version_number"].as_str().map(String::from)
}

/// One `/products` page: its add-ons and the next page's URL.
pub fn parse_page(body: &str) -> Result<(Vec<Addon>, Option<String>), String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| "marketplace reply is not JSON".to_string())?;
    let items = v["results"].as_array().ok_or("marketplace reply has no results")?;
    let addons = items
        .iter()
        .filter_map(|p| {
            let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
            let slug = p["slug"].as_str()?;
            Some(Addon {
                id: s("id"),
                name: s("name"),
                org: s("org_name"),
                extension: s("extension"),
                summary: summary(p["description"].as_str().unwrap_or("")),
                url: format!("{SITE}/product/{slug}"),
                variants: p["variants"].as_array().into_iter().flatten().filter_map(|x| x["name"].as_str().map(String::from)).collect(),
                variant_ids: p["variants"].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str().map(String::from)).collect(),
                thumbnail: p["thumbnail_cdn"].as_str().filter(|u| !u.is_empty()).map(String::from),
                version: None,
            })
        })
        .collect();
    Ok((addons, v["next"].as_str().map(String::from)))
}

/// First non-empty paragraph of a Markdown description, markup stripped.
fn summary(md: &str) -> String {
    let md = md.replace("\r\n", "\n");
    let para = md.split("\n\n").map(str::trim).find(|p| !p.is_empty() && !p.starts_with('#')).unwrap_or("");
    let plain: String = para.replace(['*', '_', '`', '#'], "").split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.chars().count() > 180 {
        format!("{}…", plain.chars().take(177).collect::<String>().trim_end())
    } else {
        plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"{"count":2,"next":"https://mp-gateway.elgato.com/products?application=wave-link&limit=1&offset=1","results":[
      {"id":"abc","name":"Old Radio","org_name":"Elgato","extension":"presets","slug":"old-radio-abc",
       "description":"**Old Radio** gives your voice a\r\nvintage tone.\r\n\r\nMore text.","variants":[{"name":"default","id":"v1"}]},
      {"id":"def","name":"Some FX","org_name":"X","extension":"vst","slug":"some-fx-def","description":"","variants":[{"name":"Windows"},{"name":"macOS"}]}]}"#;

    #[test]
    fn parses_page() {
        let (a, next) = parse_page(PAGE).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].url, "https://marketplace.elgato.com/product/old-radio-abc");
        assert_eq!(a[0].summary, "Old Radio gives your voice a vintage tone.");
        assert!(a[0].runs_on_linux());
        assert!(!a[1].runs_on_linux());
        assert_eq!(a[1].variants, ["Windows", "macOS"]);
        assert!(next.unwrap().ends_with("offset=1"));
        assert_eq!(a[0].thumbnail, None);
        assert_eq!(a[0].variant_ids, ["v1"]);
    }

    #[test]
    fn session_cookies() {
        let mut s = Session { cookies: vec![("kc_user".into(), "u".into()), ("__Host-kc_at".into(), "a1".into()), ("__stripe_mid".into(), "x".into())] };
        assert_eq!(s.header(), "kc_user=u; __Host-kc_at=a1");
        assert!(s.update(&["__Host-kc_at=a2; Path=/; Secure; HttpOnly".into()]));
        assert!(!s.update(&["__Host-kc_at=a2; Path=/".into()]));
        assert_eq!(s.cookies[1].1, "a2");
        assert_eq!(download_variant(&mut Session::default(), "abc").unwrap_err(), InstallError::SignIn);
        assert!(matches!(download_variant(&mut s, "../x"), Err(InstallError::Other(_))));
        assert_eq!(percent_decode("Retro's%20Cinema%20EQ-1.xml"), "Retro's Cinema EQ-1.xml");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn versions() {
        let d = r#"{"variants":[{"name":"Windows","is_default":false,"latest_version_number":"1.0.1.100"},
                                  {"name":"default","is_default":true,"latest_version_number":"1.0.2"}]}"#;
        assert_eq!(latest_version(d).as_deref(), Some("1.0.2"));
        assert_eq!(latest_version("{}"), None);
        assert!(get_bytes("https://example.com/x.png").is_err());
    }
}
