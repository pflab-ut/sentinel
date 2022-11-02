pub fn mask_of<T: num::One + num::FromPrimitive + std::ops::Shl<Output = T>>(i: i32) -> T {
    T::one() << T::from_i32(i).unwrap()
}

pub fn msb(n: u64) -> i8 {
    for i in (0..64).rev() {
        if n & (1u64 << i) != 0 {
            return i;
        }
    }
    -1
}
