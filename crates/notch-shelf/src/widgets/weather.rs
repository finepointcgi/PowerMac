//! Open-Meteo (https://open-meteo.com, free, no key): request URLs,
//! response parsing, WMO weather codes → SF Symbols, and the 30 minute
//! cache rule. The HTTP fetch itself is done by the platform layer.

use serde::{Deserialize, Serialize};

use super::{Rgba, WeatherNow};

/// Forecasts are refreshed at most this often.
pub const CACHE_SECS: f64 = 30.0 * 60.0;
/// After a failure, wait this long before trying again.
pub const RETRY_SECS: f64 = 5.0 * 60.0;

/// A resolved place.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// "52.52, 13.41" / "52.52 13.41" / "-33.9,151.2" → coordinates.
pub fn parse_coords(s: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = s.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
    if parts.len() != 2 {
        return None;
    }
    let lat: f64 = parts[0].parse().ok()?;
    let lon: f64 = parts[1].parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b' ' => o.push_str("%20"),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// Geocoding search. Only the part before the first comma is searched (the
/// API matches names, not "City, Region"); the rest is used to pick among
/// the results (see [`parse_geocoding`]).
pub fn geocoding_url(query: &str) -> String {
    let name = query.split(',').next().unwrap_or(query).trim();
    format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=10&language=en&format=json", enc(name))
}

#[derive(Deserialize)]
struct GeoResp {
    #[serde(default)]
    results: Vec<GeoResult>,
}

#[derive(Deserialize)]
struct GeoResult {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    country_code: Option<String>,
    #[serde(default)]
    admin1: Option<String>,
}

/// Pick the best geocoding result for `query` ("Portland, Oregon" prefers
/// the result whose region/country matches "Oregon").
pub fn parse_geocoding(json: &str, query: &str) -> Result<Place, String> {
    let r: GeoResp = serde_json::from_str(json).map_err(|e| format!("geocoding: {e}"))?;
    let hint = query.split_once(',').map(|(_, h)| h.trim().to_lowercase()).unwrap_or_default();
    let matches = |g: &GeoResult| {
        hint.is_empty()
            || [&g.admin1, &g.country, &g.country_code].iter().any(|f| f.as_deref().is_some_and(|v| v.to_lowercase().starts_with(&hint)))
    };
    let g = r.results.iter().find(|g| matches(g)).or(r.results.first()).ok_or_else(|| format!("No place called \"{}\"", query.trim()))?;
    Ok(Place { name: g.name.clone(), lat: g.latitude, lon: g.longitude })
}

pub fn forecast_url(lat: f64, lon: f64, fahrenheit: bool) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,weather_code,is_day\
         &daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1{}",
        if fahrenheit { "&temperature_unit=fahrenheit" } else { "" }
    )
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    weather_code: u32,
    #[serde(default = "one")]
    is_day: u8,
}

fn one() -> u8 {
    1
}

#[derive(Deserialize)]
struct Daily {
    temperature_2m_max: Vec<Option<f64>>,
    temperature_2m_min: Vec<Option<f64>>,
}

pub fn parse_forecast(json: &str, place: &str, fahrenheit: bool, now: f64) -> Result<WeatherNow, String> {
    let f: Forecast = serde_json::from_str(json).map_err(|e| format!("forecast: {e}"))?;
    let t = f.current.temperature_2m;
    let high = f.daily.temperature_2m_max.first().copied().flatten().unwrap_or(t);
    let low = f.daily.temperature_2m_min.first().copied().flatten().unwrap_or(t);
    Ok(WeatherNow { place: place.to_string(), temp: t, high, low, code: f.current.weather_code, is_day: f.current.is_day != 0, fahrenheit, fetched_at: now })
}

/// Whether cached weather is still fresh.
pub fn is_fresh(fetched_at: f64, now: f64) -> bool {
    now >= fetched_at && now - fetched_at < CACHE_SECS
}

/// WMO weather interpretation codes (as used by Open-Meteo) → SF Symbol.
pub fn symbol(code: u32, is_day: bool) -> &'static str {
    match code {
        0 => {
            if is_day {
                "sun.max.fill"
            } else {
                "moon.stars.fill"
            }
        }
        1 | 2 => {
            if is_day {
                "cloud.sun.fill"
            } else {
                "cloud.moon.fill"
            }
        }
        3 => "cloud.fill",
        45 | 48 => "cloud.fog.fill",
        51 | 53 | 55 => "cloud.drizzle.fill",
        56 | 57 | 66 | 67 => "cloud.sleet.fill",
        61 | 63 => "cloud.rain.fill",
        65 => "cloud.heavyrain.fill",
        71 | 73 | 75 | 77 => "cloud.snow.fill",
        80..=82 => {
            if is_day {
                "cloud.sun.rain.fill"
            } else {
                "cloud.moon.rain.fill"
            }
        }
        85 | 86 => "cloud.snow.fill",
        95 => "cloud.bolt.rain.fill",
        96 | 99 => "cloud.bolt.fill",
        _ => "thermometer.medium",
    }
}

/// Short description of a WMO code.
pub fn describe(code: u32) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly Clear",
        2 => "Partly Cloudy",
        3 => "Cloudy",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing Drizzle",
        61 => "Light Rain",
        63 => "Rain",
        65 => "Heavy Rain",
        66 | 67 => "Freezing Rain",
        71 => "Light Snow",
        73 => "Snow",
        75 => "Heavy Snow",
        77 => "Snow Grains",
        80..=82 => "Showers",
        85 | 86 => "Snow Showers",
        95 => "Thunderstorm",
        96 | 99 => "Hail Storm",
        _ => "—",
    }
}

/// Tint for the condition symbol.
pub fn tint(code: u32, is_day: bool) -> Rgba {
    match code {
        0 | 1 if is_day => Rgba(1.0, 0.80, 0.18, 1.0),
        0 | 1 => Rgba(0.80, 0.84, 1.0, 1.0),
        2 => Rgba(1.0, 0.86, 0.45, 1.0),
        51..=67 | 80..=82 => Rgba(0.45, 0.72, 1.0, 1.0),
        95..=99 => Rgba(1.0, 0.78, 0.30, 1.0),
        _ => Rgba(0.92, 0.94, 0.98, 1.0),
    }
}

/// "18°".
pub fn temp(v: f64) -> String {
    let r = v.round();
    format!("{}°", if r == 0.0 { 0.0 } else { r })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates() {
        assert_eq!(parse_coords("52.52, 13.41"), Some((52.52, 13.41)));
        assert_eq!(parse_coords("-33.87 151.21"), Some((-33.87, 151.21)));
        assert_eq!(parse_coords("Berlin"), None);
        assert_eq!(parse_coords("95, 10"), None);
        assert_eq!(parse_coords("1,2,3"), None);
    }

    #[test]
    fn urls() {
        assert_eq!(geocoding_url("San José, CR"), "https://geocoding-api.open-meteo.com/v1/search?name=San%20Jos%C3%A9&count=10&language=en&format=json");
        let f = forecast_url(52.52, 13.41, true);
        assert!(f.starts_with("https://api.open-meteo.com/v1/forecast?latitude=52.5200&longitude=13.4100&current=temperature_2m,weather_code,is_day"));
        assert!(f.ends_with("&temperature_unit=fahrenheit"));
        assert!(!forecast_url(0.0, 0.0, false).contains("fahrenheit"));
    }

    #[test]
    fn geocoding_picks_hinted_region() {
        let json = r#"{"results":[
            {"name":"Portland","latitude":45.52,"longitude":-122.68,"country":"United States","country_code":"US","admin1":"Maine"},
            {"name":"Portland","latitude":45.5,"longitude":-122.6,"country":"United States","country_code":"US","admin1":"Oregon"}]}"#;
        assert_eq!(parse_geocoding(json, "Portland, Oregon").unwrap().lat, 45.5);
        assert_eq!(parse_geocoding(json, "Portland").unwrap().lat, 45.52);
        assert_eq!(parse_geocoding(json, "Portland, Narnia").unwrap().lat, 45.52);
        assert!(parse_geocoding(r#"{"generationtime_ms":0.1}"#, "Nowhere").unwrap_err().contains("Nowhere"));
        assert!(parse_geocoding("nope", "x").is_err());
    }

    #[test]
    fn forecast_parsing() {
        let json = r#"{"latitude":52.52,"current":{"time":"2026-10-26T12:00","interval":900,"temperature_2m":11.6,"weather_code":3,"is_day":1},
            "daily":{"time":["2026-10-26"],"temperature_2m_max":[13.9],"temperature_2m_min":[6.2]}}"#;
        let w = parse_forecast(json, "Berlin", false, 1000.0).unwrap();
        assert_eq!((w.temp, w.high, w.low, w.code, w.is_day), (11.6, 13.9, 6.2, 3, true));
        assert_eq!(w.place, "Berlin");
        assert_eq!(w.fetched_at, 1000.0);
        let night = r#"{"current":{"temperature_2m":-0.4,"weather_code":0,"is_day":0},"daily":{"temperature_2m_max":[null],"temperature_2m_min":[]}}"#;
        let w = parse_forecast(night, "X", true, 0.0).unwrap();
        assert!(!w.is_day && w.fahrenheit);
        assert_eq!((w.high, w.low), (-0.4, -0.4));
        assert_eq!(temp(w.temp), "0°");
        assert_eq!(temp(17.5), "18°");
        assert!(parse_forecast("{}", "X", false, 0.0).is_err());
    }

    #[test]
    fn cache_rule() {
        assert!(is_fresh(1000.0, 1000.0 + CACHE_SECS - 1.0));
        assert!(!is_fresh(1000.0, 1000.0 + CACHE_SECS));
        // A clock that went backwards refetches.
        assert!(!is_fresh(1000.0, 10.0));
    }

    #[test]
    fn weather_codes_to_symbols() {
        assert_eq!(symbol(0, true), "sun.max.fill");
        assert_eq!(symbol(0, false), "moon.stars.fill");
        assert_eq!(symbol(2, true), "cloud.sun.fill");
        assert_eq!(symbol(3, false), "cloud.fill");
        assert_eq!(symbol(48, true), "cloud.fog.fill");
        assert_eq!(symbol(53, true), "cloud.drizzle.fill");
        assert_eq!(symbol(63, true), "cloud.rain.fill");
        assert_eq!(symbol(65, true), "cloud.heavyrain.fill");
        assert_eq!(symbol(67, true), "cloud.sleet.fill");
        assert_eq!(symbol(75, true), "cloud.snow.fill");
        assert_eq!(symbol(81, false), "cloud.moon.rain.fill");
        assert_eq!(symbol(95, true), "cloud.bolt.rain.fill");
        assert_eq!(symbol(99, true), "cloud.bolt.fill");
        assert_eq!(symbol(1234, true), "thermometer.medium");
        // Every documented code has a description.
        for c in [0, 1, 2, 3, 45, 48, 51, 53, 55, 56, 57, 61, 63, 65, 66, 67, 71, 73, 75, 77, 80, 81, 82, 85, 86, 95, 96, 99] {
            assert_ne!(describe(c), "—", "{c}");
            assert_ne!(symbol(c, true), "thermometer.medium", "{c}");
        }
    }
}
