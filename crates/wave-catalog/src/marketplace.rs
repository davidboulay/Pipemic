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
            None => return Ok(out),
        }
    }
    Ok(out)
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
       "description":"**Old Radio** gives your voice a\r\nvintage tone.\r\n\r\nMore text.","variants":[{"name":"default"}]},
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
    }
}
