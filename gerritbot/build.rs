use vergen::{generate_cargo_keys, ConstantsFlags};

fn main() {
    generate_cargo_keys(
        ConstantsFlags::SHA | ConstantsFlags::BUILD_DATE | ConstantsFlags::TARGET_TRIPLE,
    )
    .expect("Unable to generate cargo keys!");
}
