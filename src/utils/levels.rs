pub fn xp_required_for_level(level: i32) -> i32 {
    let base_xp = 20;
    let exponent = 1.3;
    (base_xp as f64 * (level as f64).powf(exponent)) as i32
}

#[test]
fn test_xp_formula() {
    println!("1   : {}", xp_required_for_level(1));
    println!("5   : {}", xp_required_for_level(5));
    println!("10  : {}", xp_required_for_level(10));
    println!("20  : {}", xp_required_for_level(20));
    println!("30  : {}", xp_required_for_level(30));
    println!("40  : {}", xp_required_for_level(40));
    println!("50  : {}", xp_required_for_level(50));
    println!("100 : {}", xp_required_for_level(100));
    println!("200 : {}", xp_required_for_level(200));
}
