use sqlx::{PgPool, Row};
use sqlx::types::Json;
use askama::Template;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::collections::{HashMap, HashSet};

#[derive(serde::Serialize, Clone)]
pub struct FAQPair {
    pub question: String,
    pub answer: String,
}

#[derive(Clone, Debug)]
pub struct AdSenseConfig {
    pub client_id: Option<String>,
    pub top_banner_slot: Option<String>,
    pub sidebar_slot: Option<String>,
}

impl AdSenseConfig {
    pub fn load_from_env() -> Self {
        Self {
            client_id: std::env::var("ADSENSE_CLIENT_ID").ok(),
            top_banner_slot: std::env::var("ADSENSE_TOP_BANNER_SLOT").ok(),
            sidebar_slot: std::env::var("ADSENSE_SIDEBAR_SLOT").ok(),
        }
    }
}

#[derive(Template)]
#[template(path = "business_page.html")]
struct BusinessPageTemplate<'a> {
    name: &'a str,
    phone: &'a str,
    website: &'a str,
    cabinet_construction: &'a str,
    min_job_size: &'a str,
    materials: Vec<String>,
    specialties: Vec<String>,
    target_city: &'a str,
    target_state: &'a str,
    schema_type: &'a str,
    rating_value: f32,
    review_count: i32,
    intro_text: String,
    meta_description: String,
    about_text: String,
    faq_schema: String,
    faq: Vec<FAQPair>,
    network_name: &'a str,
    network_domain: &'a str,
    geo_verified_local: bool,
    ccph_licensed: bool,
    city_slug: String,
    niche_slug: String,
    // Operational matrix bindings for trust checks
    has_24_7_emergency: bool,
    offers_financing: bool,
    offers_free_estimates: bool,
    is_family_owned: bool,
    eco_friendly_practices: bool,
    handles_septic: bool,
    heat_pump_specialist: bool,
    cabinet_refacing: bool,
    serves_residential: bool,
    serves_commercial: bool,
    explicit_warranties_mentioned: bool,
    years_in_business: Option<i64>,
    // AdSense configuration
    adsense_client: Option<String>,
    adsense_top_banner_slot: Option<String>,
    adsense_sidebar_slot: Option<String>,
}

#[derive(Template)]
#[template(path = "sitemap.xml")]
struct SitemapTemplate {
    urls: Vec<String>,
}

// Struct to represent individual contractors on the directory homepage
#[derive(Clone)]
struct IndexBusiness {
    name: String,
    slug: String,
    phone: String,
    display_phone: String,
    website: String,
    email: String, // Dynamic email contact field
    cabinet_construction: String,
    specialties: Vec<String>,
    system_certifications: Vec<String>,
    service_scope: Vec<String>,
    county_licensed: bool,
    poop_smart: bool,
    offers_financing: bool,
    free_estimates: bool,
    emergency_services_24_7: bool,
    years_in_business: Option<i64>,
    monetization_score: i64,
    geo_verified_local: bool,
    ccph_licensed: bool,
    // Operational matrix bindings for comparisons
    has_24_7_emergency: bool,
    has_financing: bool,
    has_free_estimates: bool,
    is_family_owned: bool,
    eco_friendly: bool,
    handles_septic: bool,
    heat_pump_specialist: bool,
    cabinet_refacing: bool,
    serves_residential: bool,
    serves_commercial: bool,
    explicit_warranties: bool,
}

// Struct representing a category grouping for the homepage
struct CategoryBlock {
    category_name: String,
    category_slug: String,
    businesses: Vec<IndexBusiness>,
}

#[derive(Template)]
#[template(path = "market_home.html")]
struct MarketHomeTemplate {
    network_name: String,
    network_domain: String,
    cities: Vec<CityLink>,
}

struct CityLink {
    name: String,
    slug: String,
}

#[derive(Template)]
#[template(path = "city_hub.html")]
struct CityHubTemplate {
    network_name: String,
    network_domain: String,
    target_city: String,
    target_state: String,
    city_slug: String,
    categories: Vec<CategoryBlock>,
    // AdSense configuration
    adsense_client: Option<String>,
    adsense_top_banner_slot: Option<String>,
    adsense_sidebar_slot: Option<String>,
}

#[derive(Template)]
#[template(path = "niche_pillar.html")]
struct NichePillarTemplate {
    network_name: String,
    network_domain: String,
    target_city: String,
    target_state: String,
    city_slug: String,
    niche_slug: String,
    category_name: String,
    businesses: Vec<IndexBusiness>,
    total_verified: usize,
    total_licensed: usize,
    faq: Vec<FAQPair>,
    meta_description: String,
    // AdSense configuration
    adsense_client: Option<String>,
    adsense_top_banner_slot: Option<String>,
    adsense_sidebar_slot: Option<String>,
}

use crate::MarketConfig;

pub async fn compile_static_assets(pool: &PgPool, config: &MarketConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("[Compiler] Initiating Phase 5 (Static Asset & SEO Compilation) for market {}...", config.target_city);

    let adsense_config = AdSenseConfig::load_from_env();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // Fetch all contractors processed past state 2 to list on homepage, including service_category, license number and operational matrix
    let rows = sqlx::query(
        "SELECT id, contractor_license_number, legal_entity_name, phone_number, website_url, enriched_data, service_category, operational_matrix FROM contractors WHERE lifecycle_state >= 2 AND target_city = $1"
    )
    .bind(&config.target_city)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        println!("[Compiler] No records sitting at lifecycle_state >= 2 found for market {}. Compilation complete.", config.target_city);
        return Ok(());
    }
    let domain = config.network_domain.as_deref().unwrap_or("niche-directory.com");
    let dist_dir = Path::new(manifest_dir).join("dist").join(domain);

    // Dynamic full clean-up at startup to avoid "ghost" templates
    if dist_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&dist_dir) {
            eprintln!("[Compiler Warning] Failed to perform directory purge: {}. Continuing...", e);
        }
    }
    fs::create_dir_all(&dist_dir)?;

    // Create static/js directory and write dispatcher_client.js
    let js_dir = dist_dir.join("static").join("js");
    fs::create_dir_all(&js_dir)?;
    let js_content = include_str!("../static/js/dispatcher_client.js");
    fs::write(js_dir.join("dispatcher_client.js"), js_content)?;

    let mut generated_urls = Vec::new();
    let mut groups: HashMap<String, Vec<IndexBusiness>> = HashMap::new(); // Collect directory data by category

    // Deduplication index tracking sets
    let mut seen_domains = HashSet::new();
    let mut seen_slugs = HashSet::new();
    let mut seen_phones = HashSet::new();

    let city_slug = slugify(&config.target_city);

    // Push market home and city hub to generated urls list
    generated_urls.push(format!("https://{}/", domain));
    generated_urls.push(format!("https://{}/{}/", domain, city_slug));

    for row in rows {
        let row_id: String = row.get("id");
        let contractor_license_number: Option<String> = row.get("contractor_license_number");
        let name: String = row.get("legal_entity_name");
        let phone: Option<String> = row.get("phone_number");
        let website: Option<String> = row.get("website_url");
        let db_enriched_json = row.get::<Option<Json<Value>>, _>("enriched_data");
        let service_category: String = row.get("service_category");
        let matrix_json = row.get::<Option<Json<Value>>, _>("operational_matrix");

        let name_trimmed = name.trim();
        let name_lower = name_trimmed.to_lowercase();
        
        // 1. Strict Business Name Validation Guard: skip if empty or generic placeholder
        if name_trimmed.is_empty() 
            || name_lower.contains("unknown") 
            || name_lower.contains("error") 
            || name_lower == "unknown contractor" 
        {
            println!("[Compiler Warning] Skipping business (ID: {}) due to invalid/generic name: '{}'", row_id, name);
            continue;
        }

        // 2. Strict Contact Validation Guard: must have a phone and a website link
        let clean_phone = phone.as_deref().unwrap_or("Unknown").trim();
        let clean_website = website.as_deref().unwrap_or("#").trim();

        if clean_phone == "Unknown" || clean_phone.is_empty() || clean_website == "#" || clean_website.is_empty() {
            println!("[Compiler Warning] Skipping business '{}' (ID: {}) due to missing phone or website contact info.", name, row_id);
            continue;
        }

        // 3. Compiler-level Deduplication Guard: filter out duplicate domains, phone numbers, or name slugs
        let domain_opt = website.as_ref().and_then(|w| extract_domain(w));
        if let Some(ref dom) = domain_opt {
            if !dom.is_empty() && seen_domains.contains(dom) {
                println!("[Compiler Warning] Skipping duplicate domain '{}' for business '{}' (ID: {})", dom, name, row_id);
                continue;
            }
        }

        let merchant_slug = slugify(&name);
        if seen_slugs.contains(&merchant_slug) {
            println!("[Compiler Warning] Skipping duplicate slug/name '{}' for business '{}' (ID: {})", merchant_slug, name, row_id);
            continue;
        }

        let normalized_phone = clean_phone.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
        if !normalized_phone.is_empty() && normalized_phone != "unknown" {
            if seen_phones.contains(&normalized_phone) {
                println!("[Compiler Warning] Skipping duplicate phone '{}' for business '{}' (ID: {})", clean_phone, name, row_id);
                continue;
            }
        }

        // Insert unique keys into tracking sets for subsequent iterations
        if let Some(dom) = domain_opt {
            if !dom.is_empty() {
                seen_domains.insert(dom);
            }
        }
        seen_slugs.insert(merchant_slug.clone());
        if !normalized_phone.is_empty() && normalized_phone != "unknown" {
            seen_phones.insert(normalized_phone);
        }

        let enriched: Value = db_enriched_json.map(|j| j.0).unwrap_or_else(|| serde_json::json!({}));
        let matrix: Value = matrix_json.map(|j| j.0).unwrap_or_else(|| serde_json::json!({}));

        let geo_verified_local = true;
        let mut ccph_licensed = false;
        if let Some(ref lic) = contractor_license_number {
            if lic.to_uppercase().contains("CCPH") {
                ccph_licensed = true;
            }
        }
        let raw_markdown = enriched.get("raw_markdown").and_then(|v| v.as_str()).unwrap_or("");
        if raw_markdown.to_uppercase().contains("CCPH") {
            ccph_licensed = true;
        }

        // Extract operational matrix properties safely
        let has_24_7_emergency = matrix.get("core_features")
            .and_then(|v| v.get("has_24_7_emergency"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let offers_financing = matrix.get("core_features")
            .and_then(|v| v.get("offers_financing"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let offers_free_estimates = matrix.get("core_features")
            .and_then(|v| v.get("offers_free_estimates"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let is_family_owned = matrix.get("core_features")
            .and_then(|v| v.get("is_family_owned"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let eco_friendly_practices = matrix.get("core_features")
            .and_then(|v| v.get("eco_friendly_practices"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let handles_septic = matrix.get("niche_features")
            .and_then(|v| v.get("handles_septic"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let heat_pump_specialist = matrix.get("niche_features")
            .and_then(|v| v.get("heat_pump_specialist"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let cabinet_refacing = matrix.get("niche_features")
            .and_then(|v| v.get("cabinet_refacing"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let serves_residential = matrix.get("commercial_focus")
            .and_then(|v| v.get("serves_residential"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let serves_commercial = matrix.get("commercial_focus")
            .and_then(|v| v.get("serves_commercial"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let explicit_warranties_mentioned = matrix.get("trust_signals")
            .and_then(|v| v.get("explicit_warranties_mentioned"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let years_in_business_matrix = matrix.get("trust_signals")
            .and_then(|v| v.get("years_in_business"))
            .and_then(|v| v.as_i64());

        let cabinet_construction = enriched.get("cabinet_construction_type").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let min_job_size = enriched.get("minimum_job_size_or_price").and_then(|v| v.as_str()).unwrap_or("Not Specified");
        let email = enriched.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
        
        let materials: Vec<String> = enriched.get("primary_materials_mentioned")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let specialties: Vec<String> = enriched.get("specialty_techniques")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        // Extract specialized OSS Septic System fields dynamically from JSON Moat
        let system_certifications: Vec<String> = enriched.get("system_certifications")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let service_scope: Vec<String> = enriched.get("service_scope")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let county_licensed = enriched.get("county_licensed").and_then(|v| v.as_bool()).unwrap_or(false);
        let poop_smart = enriched.get("poop_smart_participant").and_then(|v| v.as_bool()).unwrap_or(false);
        let offers_financing = enriched.get("offers_financing").and_then(|v| v.as_bool()).unwrap_or(false);
        let free_estimates = enriched.get("free_estimates").and_then(|v| v.as_bool()).unwrap_or(false);
        let emergency_services_24_7 = enriched.get("emergency_services_24_7").and_then(|v| v.as_bool()).unwrap_or(false);
        let years_in_business = enriched.get("years_in_business").and_then(|v| v.as_i64()).or(years_in_business_matrix);
        let monetization_score = enriched.get("monetization_score").and_then(|v| v.as_i64()).unwrap_or(0);

        let locally_owned = enriched.get("locally_owned").and_then(|v| v.as_bool()).unwrap_or(false);

        let local_license_areas: Vec<String> = enriched.get("local_license_areas")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let primary_project_specialties: Vec<String> = enriched.get("primary_project_specialties")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let niche_slug = slugify(&service_category);
        
        let url = format!("https://{}/{}/{}/{}/", domain, city_slug, niche_slug, merchant_slug);
        generated_urls.push(url.clone());

        let mut certs = system_certifications;
        let mut scope = service_scope;
        let mut specs = specialties.clone();

        // Truncate badge lists to at most 3 total combined items
        let mut total = 0;
        certs.truncate(3);
        total += certs.len();

        if total >= 3 {
            scope.clear();
            specs.clear();
        } else {
            scope.truncate(3 - total);
            total += scope.len();
            if total >= 3 {
                specs.clear();
            } else {
                specs.truncate(3 - total);
            }
        }

        let display_phone = format_phone_number(clean_phone);

        // Collect contractor records into the HashMap grouped by category
        groups.entry(service_category.clone()).or_default().push(IndexBusiness {
            name: name.clone(),
            slug: merchant_slug.clone(),
            phone: clean_phone.to_string(),
            display_phone,
            website: clean_website.to_string(),
            email,
            cabinet_construction: cabinet_construction.to_string(),
            specialties: specs,
            system_certifications: certs,
            service_scope: scope,
            county_licensed,
            poop_smart,
            offers_financing,
            free_estimates,
            emergency_services_24_7,
            years_in_business,
            monetization_score,
            geo_verified_local,
            ccph_licensed,
            has_24_7_emergency,
            has_financing: offers_financing,
            has_free_estimates: free_estimates,
            is_family_owned,
            eco_friendly: eco_friendly_practices,
            handles_septic,
            heat_pump_specialist,
            cabinet_refacing,
            serves_residential,
            serves_commercial,
            explicit_warranties: explicit_warranties_mentioned,
        });

        let schema_type = match service_category.as_str() {
            "plumbing" => "Plumber",
            "electrical" => "Electrician",
            "hvac" => "HVACBusiness",
            "kitchen_remodel" => "GeneralContractor",
            "septic_system" => "Plumber",
            "pest_control" => "LocalBusiness",
            _ => "LocalBusiness",
        };

        let rating_value = enriched.get("rating_value")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32)
            .unwrap_or(4.8);

        let review_count = enriched.get("review_count")
            .and_then(|v| v.as_i64())
            .map(|i| i as i32)
            .unwrap_or(42);

        let variations = [
            "{name} is a top-rated {category} professional serving the {city} area.",
            "Looking for reliable {category} services in {city}? {name} provides vetted, high-quality solutions.",
            "Connect with {name}, a trusted {category} specialist operating throughout {city}.",
            "Residents of {city} rely on {name} for verified, high-performance {category} expertise."
        ];

        let nice_category = match service_category.as_str() {
            "plumbing" => "plumbing",
            "electrical" => "electrical",
            "hvac" => "HVAC",
            "kitchen_remodel" => "kitchen remodeling",
            "septic_system" => "septic system",
            "pest_control" => "pest control",
            _ => &service_category,
        };

        let idx = name.len() % variations.len();
        let intro_text = variations[idx]
            .replace("{name}", &name)
            .replace("{category}", nice_category)
            .replace("{city}", &config.target_city);

        let meta_variations = [
            "Need {category} in {city}? {name} provides verified, top-rated local services. Get free estimates and view credentials today.",
            "{name} is a trusted {category} specialist serving {city}. Review their local scorecard, capabilities, and contact information.",
            "Looking for a vetted {category} in {city}? {name} offers premium services. Click to view contact details, specialties, and ratings.",
            "Find top-rated {category} services in {city} with {name}. Check their specialties, L&I verification, and contact them today.",
            "Connect with {name} for professional {category} in {city}. Vetted solutions, verified ratings, and custom project quotes."
        ];

        let meta_idx = (name.len() + 1) % meta_variations.len();
        let meta_description = meta_variations[meta_idx]
            .replace("{name}", &name)
            .replace("{category}", nice_category)
            .replace("{city}", &config.target_city);

        let about_variations = [
            "When it comes to {category} in {city}, {name} has built a reputation for reliable, high-quality execution. Whether you need immediate emergency response or a long-term project consultation, their vetted team is ready to deliver.",
            "Operating locally within {city}, {name} specializes in comprehensive {category} solutions. They maintain a strong focus on customer satisfaction and technical excellence, making them a premier choice for residents and businesses alike.",
            "With years of experience serving {city}, {name} is dedicated to providing industry-leading {category} expertise. Their commitment to safety, efficiency, and professional workmanship ensures your project is in reliable hands.",
            "If you are searching for a qualified {category} team in the {city} region, look no further than {name}. They offer tailored services, high-quality craftsmanship, and transparent pricing models for all local clients.",
            "{name} stands as a trusted resource for {category} projects throughout {city}. Known for their attention to detail and dependable service, they consistently work to meet and exceed local building and performance standards."
        ];

        let about_idx = (name.len() + 2) % about_variations.len();
        let about_text = about_variations[about_idx]
            .replace("{name}", &name)
            .replace("{category}", nice_category)
            .replace("{city}", &config.target_city);

        let faq_list = generate_faq(
            &name,
            &service_category,
            nice_category,
            &config.target_city,
            &config.target_state,
            emergency_services_24_7,
            free_estimates,
            locally_owned,
            &primary_project_specialties,
            &local_license_areas,
        );

        let faq_json_ld = serde_json::json!({
            "@context": "https://schema.org",
            "@type": "FAQPage",
            "mainEntity": faq_list.iter().map(|item| {
                serde_json::json!({
                    "@type": "Question",
                    "name": &item.question,
                    "acceptedAnswer": {
                        "@type": "Answer",
                        "text": &item.answer
                    }
                })
            }).collect::<Vec<serde_json::Value>>()
        });

        let faq_schema = serde_json::to_string(&faq_json_ld).unwrap_or_default();

        let tpl = BusinessPageTemplate {
            name: &name,
            phone: clean_phone,
            website: clean_website,
            cabinet_construction,
            min_job_size,
            materials,
            specialties,
            target_city: &config.target_city,
            target_state: &config.target_state,
            schema_type,
            rating_value,
            review_count,
            intro_text,
            meta_description,
            about_text,
            faq_schema,
            faq: faq_list,
            network_name: &config.network_name,
            network_domain: domain,
            geo_verified_local,
            ccph_licensed,
            city_slug: city_slug.clone(),
            niche_slug: niche_slug.clone(),
            has_24_7_emergency,
            offers_financing,
            offers_free_estimates,
            is_family_owned,
            eco_friendly_practices,
            handles_septic,
            heat_pump_specialist,
            cabinet_refacing,
            serves_residential,
            serves_commercial,
            explicit_warranties_mentioned,
            years_in_business,
            adsense_client: adsense_config.client_id.clone(),
            adsense_top_banner_slot: adsense_config.top_banner_slot.clone(),
            adsense_sidebar_slot: adsense_config.sidebar_slot.clone(),
        };

        let html = tpl.render()?;
        
        // Output inside nested directory structure dist/[city-slug]/[niche-slug]/[merchant-slug]/index.html
        let merchant_dir = dist_dir.join(&city_slug).join(&niche_slug).join(&merchant_slug);
        if let Err(e) = fs::create_dir_all(&merchant_dir) {
            eprintln!("[Compiler Warning] Failed to create directory {}: {}", merchant_dir.display(), e);
            continue;
        }
        
        let file_path = merchant_dir.join("index.html");
        fs::write(&file_path, html)?;

        sqlx::query("UPDATE contractors SET lifecycle_state = 4 WHERE id = $1")
            .bind(&row_id)
            .execute(pool)
            .await?;

        println!("[Compiler] Generated {} for ID {}", file_path.display(), row_id);
    }

    // Convert the HashMap into a structured vector of CategoryBlocks with deterministic sorting
    let category_names = [
        ("kitchen_remodel", "Kitchen Remodeling"),
        ("plumbing", "Plumbing Specialists"),
        ("electrical", "Electrical Services"),
        ("pest_control", "Pest Control & Extermination"),
        ("septic_system", "On-Site Septic System (OSS) Services"),
    ];

    let mut groups_back = HashMap::new();
    let mut groups_mut = groups;
    
    // Compile dynamic Niche Category Pillars: dist/[city-slug]/[niche-slug]/index.html
    for (cat_id, cat_name) in category_names {
        if let Some(mut businesses) = groups_mut.remove(cat_id) {
            businesses.sort_by(|a, b| b.monetization_score.cmp(&a.monetization_score));
            
            let niche_slug = slugify(cat_id);
            let niche_dir = dist_dir.join(&city_slug).join(&niche_slug);
            if let Err(e) = fs::create_dir_all(&niche_dir) {
                eprintln!("[Compiler Warning] Failed to create niche directory {}: {}", niche_dir.display(), e);
                continue;
            }

            let total_verified = businesses.len();
            let total_licensed = businesses.iter().filter(|b| b.ccph_licensed || b.county_licensed).count();

            let nice_category = match cat_id {
                "plumbing" => "plumbing",
                "electrical" => "electrical",
                "hvac" => "HVAC",
                "kitchen_remodel" => "kitchen remodeling",
                "septic_system" => "septic system",
                "pest_control" => "pest control",
                _ => cat_id,
            };

            let niche_faq = vec![
                FAQPair {
                    question: format!("How do I find a reliable {} in {}?", nice_category, config.target_city),
                    answer: format!("You can browse verified providers in our directory. All listed firms have passed licensing check audits and physical geolocation verification in the {} area.", config.target_city),
                },
                FAQPair {
                    question: format!("Are the {} contractors in {} licensed?", nice_category, config.target_city),
                    answer: format!("Yes. General contractors and specialty trades must be registered with L&I. Our directory displays badges for CCPH licensing validation to confirm active compliance."),
                },
            ];

            let meta_description = format!("Browse vetted, top-rated {} specialists in {}, {}. View licensing status, contact numbers, and comparison matrices.", nice_category, config.target_city, config.target_state);

            let pillar_tpl = NichePillarTemplate {
                network_name: config.network_name.clone(),
                network_domain: domain.to_string(),
                target_city: config.target_city.clone(),
                target_state: config.target_state.clone(),
                city_slug: city_slug.clone(),
                niche_slug: niche_slug.clone(),
                category_name: cat_name.to_string(),
                businesses: businesses.clone(),
                total_verified,
                total_licensed,
                faq: niche_faq,
                meta_description,
                adsense_client: adsense_config.client_id.clone(),
                adsense_top_banner_slot: adsense_config.top_banner_slot.clone(),
                adsense_sidebar_slot: adsense_config.sidebar_slot.clone(),
            };

            match pillar_tpl.render() {
                Ok(html) => {
                    let pillar_path = niche_dir.join("index.html");
                    if let Err(e) = fs::write(&pillar_path, html) {
                        eprintln!("[Compiler Warning] Failed to write niche pillar page: {}", e);
                    } else {
                        println!("[Compiler] Generated niche pillar page: {}", pillar_path.display());
                        generated_urls.push(format!("https://{}/{}/{}/", domain, city_slug, niche_slug));
                    }
                }
                Err(e) => {
                    eprintln!("[Compiler Error] Failed to render niche pillar template: {}", e);
                }
            }

            groups_back.insert(cat_id.to_string(), businesses);
        }
    }

    // Catch-all for any other categories
    for (cat_id, mut businesses) in groups_mut {
        businesses.sort_by(|a, b| b.monetization_score.cmp(&a.monetization_score));
        
        let niche_slug = slugify(&cat_id);
        let niche_dir = dist_dir.join(&city_slug).join(&niche_slug);
        if let Err(e) = fs::create_dir_all(&niche_dir) {
            eprintln!("[Compiler Warning] Failed to create directory: {}", e);
            continue;
        }

        let total_verified = businesses.len();
        let total_licensed = businesses.iter().filter(|b| b.ccph_licensed || b.county_licensed).count();
        
        let cat_name = cat_id.replace('_', " ");
        let cat_name = cat_name.chars().next().unwrap_or(' ').to_uppercase().to_string() + &cat_name[1..];

        let niche_faq = vec![
            FAQPair {
                question: format!("Are {} specialists in {} licensed?", cat_name, config.target_city),
                answer: format!("All local contractors must be registered and bonded. Our directory highlights verified providers."),
            }
        ];

        let meta_description = format!("Search verified {} contractors in {}, {}.", cat_name, config.target_city, config.target_state);

        let pillar_tpl = NichePillarTemplate {
            network_name: config.network_name.clone(),
            network_domain: domain.to_string(),
            target_city: config.target_city.clone(),
            target_state: config.target_state.clone(),
            city_slug: city_slug.clone(),
            niche_slug: niche_slug.clone(),
            category_name: cat_name.to_string(),
            businesses: businesses.clone(),
            total_verified,
            total_licensed,
            faq: niche_faq,
            meta_description,
            adsense_client: adsense_config.client_id.clone(),
            adsense_top_banner_slot: adsense_config.top_banner_slot.clone(),
            adsense_sidebar_slot: adsense_config.sidebar_slot.clone(),
        };

        if let Ok(html) = pillar_tpl.render() {
            let _ = fs::write(niche_dir.join("index.html"), html);
            generated_urls.push(format!("https://{}/{}/{}/", domain, city_slug, niche_slug));
        }

        groups_back.insert(cat_id.to_string(), businesses);
    }

    // Convert groups back to CategoryBlocks
    let mut categories = Vec::new();
    for (cat_id, cat_name) in category_names {
        if let Some(businesses) = groups_back.remove(cat_id) {
            categories.push(CategoryBlock {
                category_name: cat_name.to_string(),
                category_slug: slugify(cat_id),
                businesses,
            });
        }
    }

    // Compile and write Global/Market Home page: dist/index.html
    let market_home_tpl = MarketHomeTemplate {
        network_name: config.network_name.clone(),
        network_domain: domain.to_string(),
        cities: vec![CityLink {
            name: config.target_city.clone(),
            slug: city_slug.clone(),
        }],
    };
    if let Ok(market_home_html) = market_home_tpl.render() {
        let index_path = dist_dir.join("index.html");
        let _ = fs::write(&index_path, market_home_html);
        println!("[Compiler] Generated Global/Market Home page: {}", index_path.display());
    }

    // Compile and write City Hub page: dist/[city-slug]/index.html
    let city_hub_tpl = CityHubTemplate {
        network_name: config.network_name.clone(),
        network_domain: domain.to_string(),
        target_city: config.target_city.clone(),
        target_state: config.target_state.clone(),
        city_slug: city_slug.clone(),
        categories,
        adsense_client: adsense_config.client_id.clone(),
        adsense_top_banner_slot: adsense_config.top_banner_slot.clone(),
        adsense_sidebar_slot: adsense_config.sidebar_slot.clone(),
    };
    
    if let Ok(city_hub_html) = city_hub_tpl.render() {
        let city_hub_dir = dist_dir.join(&city_slug);
        let index_path = city_hub_dir.join("index.html");
        let _ = fs::write(&index_path, city_hub_html);
        println!("[Compiler] Generated City Hub homepage: {}", index_path.display());
    }

    // --- Generate Omni-Trap / Honeypot Labyrinth ---
    let trap_dir = dist_dir.join("trap");
    if let Err(e) = fs::create_dir_all(&trap_dir) {
        eprintln!("[Compiler Warning] Failed to create trap folder: {}", e);
    } else {
        // Write a simple trap.php IP-logger to log malicious scrapers
        let php_logger = r#"<?php
$ip = $_SERVER['REMOTE_ADDR'];
$agent = $_SERVER['HTTP_USER_AGENT'];
$time = date('Y-m-d H:i:s');
$log = "$time | IP: $ip | User-Agent: $agent | Requested: " . $_SERVER['REQUEST_URI'] . "\n";
file_put_contents('trap_log.txt', $log, FILE_APPEND);
?>
<!DOCTYPE html>
<html><head><meta name="robots" content="noindex, nofollow"></head><body></body></html>
"#;
        let _ = fs::write(trap_dir.join("trap.php"), php_logger);

        // Generate 5 recursively linked HTML honeypots with rich, randomized cabinetry/construction keywords
        let keywords = vec![
            "wholesale custom cabinets", "premium vanity builder", "cheap local plumbers",
            "emergency septic pumping", "licensed clark county electricians", "best general contractors",
            "home remodels cascade park", "affordable cabinet installers", "hvac replacement vancouver"
        ];

        for i in 1..=5 {
            let current_page = format!("trap_{}.html", i);
            let next_page = if i == 5 { "trap_1.html".to_string() } else { format!("trap_{}.html", i + 1) };
            
            let kw1 = keywords[(i * 3) % keywords.len()];
            let kw2 = keywords[(i * 3 + 1) % keywords.len()];
            let kw3 = keywords[(i * 3 + 2) % keywords.len()];

            let trap_html = format!(
                r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="robots" content="noindex, nofollow">
    <title>Directory Data Node {i}</title>
</head>
<body>
    <h1>Index Node {i} - Service Mapping</h1>
    <p>Mapping dynamic metadata references for {kw1}, {kw2}, and {kw3}.</p>
    <!-- Recursive link to bait scrapers into infinite loops -->
    <a href="/trap/{next_page}">Resolve next directory partition &rarr;</a>
    <script src="trap.php"></script>
</body>
</html>"#
            );
            let _ = fs::write(trap_dir.join(&current_page), trap_html);
        }
        println!("[Omni-Trap] Generated 5 recursively-linked honeypot pages in dist/trap/");
    }

    // Generate robots.txt file to safeguard compliant crawlers
    let robots_content = r#"User-agent: *
Disallow: /trap/

User-agent: Googlebot
Disallow: /trap/

User-agent: Bingbot
Disallow: /trap/
"#;
    let _ = fs::write(dist_dir.join("robots.txt"), robots_content);

    // Compile and write sitemap.xml
    let sitemap_tpl = SitemapTemplate { urls: generated_urls };
    match sitemap_tpl.render() {
        Ok(sitemap_xml) => {
            let sitemap_path = dist_dir.join("sitemap.xml");
            if let Err(e) = fs::write(&sitemap_path, &sitemap_xml) {
                eprintln!("[Compiler Warning] Failed to write sitemap.xml: {}", e);
            } else {
                println!("[Compiler] Generated sitemap.xml at {}", sitemap_path.display());
            }
        }
        Err(e) => {
            eprintln!("[Compiler Error] Failed to render sitemap template: {}", e);
        }
    }

    // --- IndexNow API Option 1 Verification ---
    let key = "372eb0e23b9f45419f4b4f3b9443f96e";
    let key_file_name = format!("{}.txt", key);
    let source_key_file = Path::new(manifest_dir).join("static").join(&key_file_name);
    let key_file_path = dist_dir.join(&key_file_name);
    
    let mut key_copied = false;
    if source_key_file.exists() {
        if let Err(e) = fs::copy(&source_key_file, &key_file_path) {
            eprintln!("[Compiler Error] Failed to copy IndexNow verification key file: {}", e);
        } else {
            println!("[Compiler] Copied IndexNow ownership verification file to: {}", key_file_path.display());
            key_copied = true;
        }
    } else {
        eprintln!("[Compiler Error] Source IndexNow verification file does not exist at: {}", source_key_file.display());
    }

    if key_copied {
        let key_location = format!("https://{}/{}", domain, key_file_name);
        let mut urls_to_submit = sitemap_tpl.urls.clone();
        let homepage_url = format!("https://{}/", domain);
        if !urls_to_submit.contains(&homepage_url) {
            urls_to_submit.push(homepage_url.clone());
        }

        let indexnow_payload = serde_json::json!({
            "host": domain,
            "key": key,
            "keyLocation": key_location,
            "urlList": urls_to_submit
        });

        if std::env::var("PRODUCTION").unwrap_or_default() == "true" {
            println!("[IndexNow] Pinging: https://api.indexnow.org/IndexNow?url={}&key={}", homepage_url, key);
            println!("[Compiler] Dispatching IndexNow API push-indexing submission for {} URLs...", urls_to_submit.len());
            let client = reqwest::Client::new();
            
            // Asynchronously dispatch the HTTP POST request to IndexNow
            let response_fut = client.post("https://api.indexnow.org/indexnow")
                .json(&indexnow_payload)
                .send();
                
            match response_fut.await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        println!("[Compiler] IndexNow API submission successful! Status: {}", status);
                    } else {
                        eprintln!("[Compiler Warning] IndexNow API returned error code: {}. Description: {:?}", status, resp.text().await.unwrap_or_default());
                    }
                }
                Err(e) => {
                    eprintln!("[Compiler Error] Failed to connect or send request to IndexNow API: {}", e);
                }
            }
        } else {
            println!("[Compiler] Skipping IndexNow ping - site must be deployed to live domain first to avoid 403.");
        }
    }

    println!("[Compiler] Phase 5 Completed Cleanly!");
    Ok(())
}

fn slugify(text: &str) -> String {
    let slug = text.to_lowercase().replace('_', "-").replace(|c: char| !c.is_alphanumeric() && c != '-', "");
    slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<&str>>().join("-")
}

fn extract_domain(url: &str) -> Option<String> {
    let cleaned = url.trim().to_lowercase();
    let without_protocol = if let Some(stripped) = cleaned.strip_prefix("https://") {
        stripped
    } else if let Some(stripped) = cleaned.strip_prefix("http://") {
        stripped
    } else {
        &cleaned
    };
    let without_www = without_protocol.strip_prefix("www.").unwrap_or(without_protocol);
    let parts: Vec<&str> = without_www.split('/').collect();
    parts.first().map(|s| s.to_string())
}

fn format_phone_number(phone: &str) -> String {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 11 && digits.starts_with('1') {
        let area = &digits[1..4];
        let prefix = &digits[4..7];
        let line = &digits[7..11];
        format!("({}) {}-{}", area, prefix, line)
    } else if digits.len() == 10 {
        let area = &digits[0..3];
        let prefix = &digits[3..6];
        let line = &digits[6..10];
        format!("({}) {}-{}", area, prefix, line)
    } else {
        phone.to_string()
    }
}

fn generate_faq(
    name: &str,
    category: &str,
    nice_category: &str,
    city: &str,
    state: &str,
    emergency_services_24_7: bool,
    free_estimates: bool,
    locally_owned: bool,
    primary_project_specialties: &[String],
    local_license_areas: &[String],
) -> Vec<FAQPair> {
    let mut faq = Vec::new();

    // Q1: Specialties
    let q1 = format!("What services does {} specialize in?", name);
    let a1 = if !primary_project_specialties.is_empty() {
        format!(
            "{} specializes in {}, with primary focus areas including {}.",
            name,
            nice_category,
            primary_project_specialties.join(", ")
        )
    } else {
        format!(
            "{} specializes in professional {} services, delivering high-quality workmanship for local projects.",
            name,
            nice_category
        )
    };
    faq.push(FAQPair { question: q1, answer: a1 });

    // Q2: Service Area
    let q2 = format!("What areas does {} serve around {}?", name, city);
    let a2 = if !local_license_areas.is_empty() {
        format!(
            "{} provides service across the {} area, licensed and active in {}.",
            name,
            city,
            local_license_areas.join(", ")
        )
    } else {
        format!(
            "{} serves residential and commercial properties throughout {}, {} and the surrounding neighborhoods.",
            name,
            city,
            state
        )
    };
    faq.push(FAQPair { question: q2, answer: a2 });

    // Q3: Emergency Services
    let q3 = format!("Does {} offer emergency or 24/7 {} services?", name, nice_category);
    let a3 = if emergency_services_24_7 {
        format!(
            "Yes, {} offers 24/7 emergency {} services to handle urgent repair and service requests in the {} area.",
            name,
            nice_category,
            city
        )
    } else {
        format!(
            "While {} conducts scheduled operations during standard business hours, you can reach out directly to inquire about urgent or same-day service openings.",
            name
        )
    };
    faq.push(FAQPair { question: q3, answer: a3 });

    // Q4: Estimates
    let q4 = format!("Can I request a free estimate from {}?", name);
    let a4 = if free_estimates {
        format!(
            "Yes! {} provides free estimates and initial consultations for homeowners and businesses in the {} region.",
            name,
            city
        )
    } else {
        format!(
            "For pricing inquiries, project bids, or to schedule a consultation with {}, please contact their office directly.",
            name
        )
    };
    faq.push(FAQPair { question: q4, answer: a4 });

    // Q5: Locality / Trust
    let q5 = format!("Is {} locally owned and operated?", name);
    let a5 = if locally_owned {
        format!(
            "Yes, {} is a locally owned and operated business, dedicated to supporting clients with reliable community service in {} and the surrounding areas.",
            name,
            city
        )
    } else {
        format!(
            "Yes, {} operates as a registered local service provider within the {} area, committed to high-standard regional projects.",
            name,
            city
        )
    };
    faq.push(FAQPair { question: q5, answer: a5 });

    faq
}