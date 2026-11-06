//! Elgato's firmware catalog (the endpoint Wave Link 3.4.0 for macOS uses).
//! Blocking; call from a worker thread in GUIs.
//! `marketplace`: Wave Link add-ons on the Elgato marketplace.

pub mod marketplace;

/// Catalog URL. The macOS parameters are what the app sends; the reply lists
/// the same Wave:3 firmware regardless of OS.
pub const CATALOG_URL: &str = "https://gc-updates.elgato.com/mac/ewlm-update/final/device-update-check.php\
?osName=macOS&osVersion=15.0&osPlatform=arm64&appVersion=3.4.0&appLanguage=en-US";

const VID: u64 = 0x0fd9;
const PID_WAVE3: u64 = 0x0070;

#[derive(Debug, Clone, PartialEq)]
pub struct Firmware {
    pub version: String,
    pub url: String,
    pub importance: String,
}

/// All Wave:3 firmware in the catalog, oldest first.
pub fn wave3_firmware() -> Result<Vec<Firmware>, String> {
    let body = ureq::get(CATALOG_URL)
        .call()
        .map_err(|e| format!("firmware catalog: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("firmware catalog: {e}"))?;
    parse(&body)
}

/// Newest Wave:3 firmware in the catalog.
pub fn latest_wave3() -> Result<Firmware, String> {
    wave3_firmware()?.pop().ok_or_else(|| "catalog lists no Wave:3 firmware".into())
}

pub fn parse(body: &str) -> Result<Vec<Firmware>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| "catalog is not JSON".to_string())?;
    let dev = v["hardwareDevices"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["id"]["vendorId"] == VID && d["id"]["productId"] == PID_WAVE3)
        .ok_or("Wave:3 not listed in the catalog")?;
    let mut out: Vec<Firmware> = dev["firmware"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(Firmware {
                version: f["version"].as_str()?.to_string(),
                url: f["downloadUrl"].as_str()?.to_string(),
                importance: f["importance"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    out.sort_by_key(|f| version_key(&f.version));
    Ok(out)
}

/// "1.2.2", "1.2.2 (3) 0b4251ee" or "1.2.2 build 3" -> [1, 2, 2].
pub fn version_key(s: &str) -> Vec<u32> {
    s.split_whitespace().next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Is `candidate` newer than `installed`? Both may carry build suffixes.
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    version_key(candidate) > version_key(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"appDevices":[],"hardwareDevices":[
      {"name":"Stream Deck","id":{"type":"usb","vendorId":4057,"productId":128},"firmware":[{"version":"9.9.9","downloadUrl":"x"}]},
      {"name":"Wave:3","id":{"type":"usb","vendorId":4057,"productId":112},"firmware":[
        {"type":"Base","version":"1.10.0","downloadUrl":"https://e/b.dfu","importance":"regular"},
        {"type":"Base","version":"1.2.2","downloadUrl":"https://e/a.dfu","importance":"regular"}]}]}"#;

    #[test]
    fn picks_wave3_sorted() {
        let fw = parse(SAMPLE).unwrap();
        assert_eq!(fw.iter().map(|f| f.version.as_str()).collect::<Vec<_>>(), ["1.2.2", "1.10.0"]);
        assert_eq!(fw[1].url, "https://e/b.dfu");
    }

    #[test]
    fn compares_versions() {
        assert!(!is_newer("1.2.2", "1.2.2 (3) 0b4251ee"));
        assert!(is_newer("1.2.3", "1.2.2 (3) 0b4251ee"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.2.1", "1.2.2 build 3"));
    }

    #[test]
    fn missing_device() {
        assert!(parse(r#"{"hardwareDevices":[]}"#).is_err());
    }
}
