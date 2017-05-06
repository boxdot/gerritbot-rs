pub fn transform_u64_to_array_of_u8(x: u64) -> [u8; 8] {
    let b0: u8 = ((x >> 56) & 0xff) as u8;
    let b1: u8 = ((x >> 48) & 0xff) as u8;
    let b2: u8 = ((x >> 40) & 0xff) as u8;
    let b3: u8 = ((x >> 32) & 0xff) as u8;
    let b4: u8 = ((x >> 24) & 0xff) as u8;
    let b5: u8 = ((x >> 16) & 0xff) as u8;
    let b6: u8 = ((x >> 8) & 0xff) as u8;
    let b7: u8 = (x & 0xff) as u8;
    [b0, b1, b2, b3, b4, b5, b6, b7]
}

pub fn xorshift64star(seed: u64) -> u64 {
    let mut x = seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545F4914F6CDD1Du64)
}
