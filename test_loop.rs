fn main() {
    let mut current_scrips = String::from("nse_cm|11536");
    let mut prices = std::collections::HashMap::new();
    prices.insert("nse_cm|11536".to_string(), 2198.1);
    prices.insert("nse_fo|51386".to_string(), 0.0);

    for i in 0..3 {
        let mut scrip_keys: Vec<String> = prices.iter().map(|(k, _)| k.clone()).collect();
        scrip_keys.sort();
        let new_scrips = scrip_keys.join("&");
        if new_scrips != current_scrips && !new_scrips.is_empty() {
            println!("Iter {}: old={} new={}", i, current_scrips, new_scrips);
            current_scrips = new_scrips;
        } else {
            println!("Iter {}: NO UPDATE", i);
        }
    }
}
