fn main() {
    vergen::generate_cargo_keys(vergen::ConstantsFlags::SHA)
        .expect("Unable to generate cargo keys!");
}
