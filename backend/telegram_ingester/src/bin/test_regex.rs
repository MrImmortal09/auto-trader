use regex::Regex;

fn main() {
    let text = "BUY NIFTY 24250 PE ABOVE 200\n\nTARGET :- 230 / 290\n\nSL :- 140\n\n21 JULY";
    
    let opts_re = Regex::new(
        r"(?i)(BUY|SELL)\s+([A-Z0-9&]+)\s+(\d+(?:\.\d+)?)\s+(CE|PE)\s+(ABOVE|BELOW)\s+(\d+(?:\.\d+)?)"
    ).unwrap();
    let eqt_re = Regex::new(
        r"(?i)(BUY|SELL)\s+([A-Z0-9&]+)\s+(ABOVE|BELOW)\s+(\d+(?:\.\d+)?)"
    ).unwrap();
    
    if let Some(caps) = opts_re.captures(text) {
        println!("Opts matched!");
        println!("Instrument: {}", &caps[2]);
        println!("Strike: {}", &caps[3]);
        println!("OptionType: {}", &caps[4]);
    } else if let Some(caps) = eqt_re.captures(text) {
        println!("Eqt matched!");
        println!("Instrument: {}", &caps[2]);
    } else {
        println!("None matched!");
    }
}
