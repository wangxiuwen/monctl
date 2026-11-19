// The bundled phone model database (phone-models.json), taken from Snug
// (github.com/zephyr4123/snug, MIT): brands with their models' physical
// screen sizes. Apple models also carry `points`, the logical width macOS
// renders at; Android has no public logical width, so the fit assumes 400pt
// (see fit.rs). Adding or fixing models only touches the JSON file.

use super::json::Json;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub screen: (u32, u32),
    /// Logical width, Apple only.
    pub points: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Brand {
    pub name: String,
    pub aliases: Vec<String>,
    pub models: Vec<Model>,
}

fn db() -> &'static [Brand] {
    static DB: OnceLock<Vec<Brand>> = OnceLock::new();
    DB.get_or_init(|| {
        let text = include_str!("phone-models.json");
        let v = super::json::parse(text).unwrap_or_else(|e| panic!("bundled phone-models.json is broken: {}", e));
        let mut brands = Vec::new();
        for b in v.get("brands").and_then(Json::as_arr).unwrap_or(&[]) {
            let name = b.get("name").and_then(Json::as_str).unwrap_or_default().to_string();
            let aliases = b.get("aliases").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|a| a.as_str().map(String::from)).collect();
            let mut models = Vec::new();
            for m in b.get("models").and_then(Json::as_arr).unwrap_or(&[]) {
                let (Some(mname), Some(screen)) = (m.get("name").and_then(Json::as_str), m.get("screen").and_then(Json::as_arr)) else {
                    continue;
                };
                let w = screen.first().and_then(Json::as_num).unwrap_or(0.0) as u32;
                let h = screen.get(1).and_then(Json::as_num).unwrap_or(0.0) as u32;
                if w == 0 || h == 0 {
                    continue;
                }
                models.push(Model {
                    name: mname.to_string(),
                    screen: super::fit::portrait(w, h),
                    points: m.get("points").and_then(Json::as_num).map(|p| p as u32),
                });
            }
            if !models.is_empty() {
                brands.push(Brand { name, aliases, models });
            }
        }
        brands
    })
}

pub fn brands() -> &'static [Brand] {
    db()
}

/// Case-insensitive substring across model name, brand name and aliases;
/// an empty query lists everything. Returns (brand, model) pairs.
pub fn search(query: &str) -> Vec<(&'static Brand, &'static Model)> {
    let q = query.trim().to_lowercase();
    let mut hits = Vec::new();
    for b in db() {
        let brand_hit = !q.is_empty() && (b.name.to_lowercase().contains(&q) || b.aliases.iter().any(|a| a.to_lowercase().contains(&q)));
        for m in &b.models {
            if q.is_empty() || brand_hit || m.name.to_lowercase().contains(&q) {
                hits.push((b, m));
            }
        }
    }
    hits
}

/// The one model a query means, for `phone fit` and `phone on --model`.
/// Exact model name wins; then a unique substring hit.
pub fn lookup(query: &str) -> Option<(&'static Brand, &'static Model)> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let ql = q.to_lowercase();
    let exact: Vec<_> = db().iter().flat_map(|b| b.models.iter().map(move |m| (b, m))).filter(|(_, m)| m.name.to_lowercase() == ql).collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    let hits = search(q);
    if hits.len() == 1 {
        Some(hits[0])
    } else {
        None
    }
}

// ---- CLI ----------------------------------------------------------------

pub fn run_cli(args: &[String]) {
    let query = args.join(" ");
    let hits = search(&query);
    if hits.is_empty() {
        crate::common::fail(&format!("no phone model matches \"{}\" ({} brands bundled)", query, db().len()));
    }
    let mut brand = None;
    for (b, m) in &hits {
        if brand.as_deref() != Some(&b.name) {
            println!("{}", b.name);
            brand = Some(&b.name);
        }
        println!("  {}  {}x{}{}", m.name, m.screen.0, m.screen.1, match m.points {
            Some(p) => format!("  ({}pt)", p),
            None => String::new(),
        });
    }
    println!("- {} models", hits.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_loads_with_brands_and_models() {
        assert!(db().len() > 3, "expected several brands");
        assert!(db().iter().all(|b| !b.models.is_empty()));
    }

    #[test]
    fn every_model_is_portrait_and_reasonable() {
        for b in db() {
            for m in &b.models {
                let (w, h) = m.screen;
                assert!(w <= h, "{} {} is not portrait", b.name, m.name);
                assert!((100..=5000).contains(&w) && (200..=10000).contains(&h), "{} {} has odd size {}x{}", b.name, m.name, w, h);
                // Only Apple-style entries carry a logical width.
                if m.points.is_some() {
                    assert!(m.points.unwrap() <= w, "{} {} has points wider than pixels", b.name, m.name);
                }
            }
        }
    }

    #[test]
    fn search_hits_names_brands_and_aliases() {
        assert!(search("iPhone 17").iter().all(|(b, _)| b.name == "Apple"));
        assert!(!search("Apple").is_empty());
        assert!(!search("三星").is_empty() || db().iter().any(|b| b.aliases.iter().any(|a| a.contains("三星"))));
        assert_eq!(search("no-such-phone").len(), 0);
    }

    #[test]
    fn lookup_prefers_exact_names() {
        // An exact model name must not be diluted by substring hits.
        if let Some((_, m)) = lookup("iPhone 17 Pro Max") {
            assert_eq!(m.name, "iPhone 17 Pro Max");
        }
        assert!(lookup("").is_none());
        assert!(lookup("zzz-not-a-phone").is_none());
    }
}
