use chrono::{DateTime, Local, TimeZone};

fn main() {
    let epoch = 1468506600_i64;
    let adjusted = epoch + 315532800;
    let dt = DateTime::from_timestamp(adjusted, 0).unwrap();
    let local = dt.naive_local().date();
    let local_utc = dt.naive_utc().date();
    println!("Adjusted epoch: {}", adjusted);
    println!("NaiveLocal date: {}", local);
    println!("NaiveUTC date: {}", local_utc);
}
