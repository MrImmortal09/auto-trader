fn build_scrip_byte_array(scrips: &str) -> Vec<u8> {
    let scrip_list: Vec<String> = scrips
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|s| format!("{}|{}", "sf", s))
        .collect();

    let scrips_count = scrip_list.len();
    let data_len: usize = scrip_list.iter().map(|s| s.len() + 1).sum::<usize>() + 2;
    let mut bytes = Vec::with_capacity(data_len);

    bytes.push(((scrips_count >> 8) & 0xFF) as u8);
    bytes.push((scrips_count & 0xFF) as u8);

    for scrip in &scrip_list {
        bytes.push((scrip.len() & 0xFF) as u8);
        bytes.extend_from_slice(scrip.as_bytes());
    }
    bytes
}
fn main() {
    println!("{:?}", build_scrip_byte_array("nse_cm|11536&nse_fo|51386"));
}
