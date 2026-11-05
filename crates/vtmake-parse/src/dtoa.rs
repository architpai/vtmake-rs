//! Grisu2 and Prettify from milo/dtoa_milo.h. See vendor/LICENSE.txt.
const POWERS: [(u64, i32); 87] = [
    (0xfa8fd5a0081c0288, -1220),
    (0xbaaee17fa23ebf76, -1193),
    (0x8b16fb203055ac76, -1166),
    (0xcf42894a5dce35ea, -1140),
    (0x9a6bb0aa55653b2d, -1113),
    (0xe61acf033d1a45df, -1087),
    (0xab70fe17c79ac6ca, -1060),
    (0xff77b1fcbebcdc4f, -1034),
    (0xbe5691ef416bd60c, -1007),
    (0x8dd01fad907ffc3c, -980),
    (0xd3515c2831559a83, -954),
    (0x9d71ac8fada6c9b5, -927),
    (0xea9c227723ee8bcb, -901),
    (0xaecc49914078536d, -874),
    (0x823c12795db6ce57, -847),
    (0xc21094364dfb5637, -821),
    (0x9096ea6f3848984f, -794),
    (0xd77485cb25823ac7, -768),
    (0xa086cfcd97bf97f4, -741),
    (0xef340a98172aace5, -715),
    (0xb23867fb2a35b28e, -688),
    (0x84c8d4dfd2c63f3b, -661),
    (0xc5dd44271ad3cdba, -635),
    (0x936b9fcebb25c996, -608),
    (0xdbac6c247d62a584, -582),
    (0xa3ab66580d5fdaf6, -555),
    (0xf3e2f893dec3f126, -529),
    (0xb5b5ada8aaff80b8, -502),
    (0x87625f056c7c4a8b, -475),
    (0xc9bcff6034c13053, -449),
    (0x964e858c91ba2655, -422),
    (0xdff9772470297ebd, -396),
    (0xa6dfbd9fb8e5b88f, -369),
    (0xf8a95fcf88747d94, -343),
    (0xb94470938fa89bcf, -316),
    (0x8a08f0f8bf0f156b, -289),
    (0xcdb02555653131b6, -263),
    (0x993fe2c6d07b7fac, -236),
    (0xe45c10c42a2b3b06, -210),
    (0xaa242499697392d3, -183),
    (0xfd87b5f28300ca0e, -157),
    (0xbce5086492111aeb, -130),
    (0x8cbccc096f5088cc, -103),
    (0xd1b71758e219652c, -77),
    (0x9c40000000000000, -50),
    (0xe8d4a51000000000, -24),
    (0xad78ebc5ac620000, 3),
    (0x813f3978f8940984, 30),
    (0xc097ce7bc90715b3, 56),
    (0x8f7e32ce7bea5c70, 83),
    (0xd5d238a4abe98068, 109),
    (0x9f4f2726179a2245, 136),
    (0xed63a231d4c4fb27, 162),
    (0xb0de65388cc8ada8, 189),
    (0x83c7088e1aab65db, 216),
    (0xc45d1df942711d9a, 242),
    (0x924d692ca61be758, 269),
    (0xda01ee641a708dea, 295),
    (0xa26da3999aef774a, 322),
    (0xf209787bb47d6b85, 348),
    (0xb454e4a179dd1877, 375),
    (0x865b86925b9bc5c2, 402),
    (0xc83553c5c8965d3d, 428),
    (0x952ab45cfa97a0b3, 455),
    (0xde469fbd99a05fe3, 481),
    (0xa59bc234db398c25, 508),
    (0xf6c69a72a3989f5c, 534),
    (0xb7dcbf5354e9bece, 561),
    (0x88fcf317f22241e2, 588),
    (0xcc20ce9bd35c78a5, 614),
    (0x98165af37b2153df, 641),
    (0xe2a0b5dc971f303a, 667),
    (0xa8d9d1535ce3b396, 694),
    (0xfb9b7cd9a4a7443c, 720),
    (0xbb764c4ca7a44410, 747),
    (0x8bab8eefb6409c1a, 774),
    (0xd01fef10a657842c, 800),
    (0x9b10a4e5e9913129, 827),
    (0xe7109bfba19c0c9d, 853),
    (0xac2820d9623bf429, 880),
    (0x80444b5e7aa7cf85, 907),
    (0xbf21e44003acdd2d, 933),
    (0x8e679c2f5e44ff8f, 960),
    (0xd433179d9c8cb841, 986),
    (0x9e19db92b4e31ba9, 1013),
    (0xeb96bf6ebadf77d9, 1039),
    (0xaf87023b9bf0ee6b, 1066),
];
#[derive(Clone, Copy)]
struct Diy {
    f: u64,
    e: i32,
}
impl Diy {
    fn from(v: f64) -> Self {
        let bits = v.to_bits();
        let biased = ((bits >> 52) & 2047) as i32;
        let sig = bits & ((1 << 52) - 1);
        if biased != 0 {
            Self {
                f: sig + (1 << 52),
                e: biased - 1075,
            }
        } else {
            Self { f: sig, e: -1074 }
        }
    }
    fn norm(self) -> Self {
        let n = self.f.leading_zeros();
        Self {
            f: self.f << n,
            e: self.e - n as i32,
        }
    }
    fn mul(self, b: Self) -> Self {
        let p = self.f as u128 * b.f as u128;
        Self {
            f: ((p >> 64) as u64).wrapping_add(((p as u64) >> 63) & 1),
            e: self.e + b.e + 64,
        }
    }
}
fn round(buf: &mut [u8], delta: u64, mut rest: u64, ten: u64, distance: u64) {
    while rest < distance
        && delta - rest >= ten
        && (rest + ten < distance || distance - rest > rest + ten - distance)
    {
        *buf.last_mut().unwrap() -= 1;
        rest += ten;
    }
}
/// Format a double with the same digits and exponent rules as tippecanoe.
pub fn dtoa_milo(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v < 0. { "-inf" } else { "inf" }.into();
    }
    if v == 0. {
        return "0".into();
    }
    let value = Diy::from(v.abs());
    let plus = Diy {
        f: (value.f << 1) + 1,
        e: value.e - 1,
    }
    .norm();
    let mut minus = if value.f == 1 << 52 {
        Diy {
            f: (value.f << 2) - 1,
            e: value.e - 2,
        }
    } else {
        Diy {
            f: (value.f << 1) - 1,
            e: value.e - 1,
        }
    };
    minus.f <<= minus.e - plus.e;
    minus.e = plus.e;
    let dk = (-61 - plus.e) as f64 * 0.30102999566398114 + 347.;
    let index = ((dk.ceil() as i32 >> 3) + 1) as usize;
    let (f, e) = POWERS[index];
    let cached = Diy { f, e };
    let w = value.norm().mul(cached);
    let mut wp = plus.mul(cached);
    let mut wm = minus.mul(cached);
    wp.f -= 1;
    wm.f += 1;
    let mut delta = wp.f - wm.f;
    let mut k = 348 - (index as i32 * 8);
    let one = 1u64 << -wp.e;
    let mut p1 = (wp.f >> -wp.e) as u32;
    let mut p2 = wp.f & (one - 1);
    let mut kappa = if p1 == 0 { 1 } else { p1.ilog10() + 1 } as i32;
    let mut buf = Vec::with_capacity(32);
    let distance = wp.f - w.f;
    let mut done = false;
    while kappa > 0 {
        let power = 10u32.pow((kappa - 1) as u32);
        let digit = p1 / power;
        p1 %= power;
        if digit != 0 || !buf.is_empty() {
            buf.push(b'0' + digit as u8);
        }
        kappa -= 1;
        let rest = ((p1 as u64) << -wp.e) + p2;
        if rest <= delta {
            k += kappa;
            round(
                &mut buf,
                delta,
                rest,
                (10u64.pow(kappa as u32)) << -wp.e,
                distance,
            );
            done = true;
            break;
        }
    }
    if !done {
        loop {
            p2 = p2.wrapping_mul(10);
            delta = delta.wrapping_mul(10);
            let digit = (p2 >> -wp.e) as u8;
            if digit != 0 || !buf.is_empty() {
                buf.push(b'0' + digit);
            }
            p2 &= one - 1;
            kappa -= 1;
            if p2 < delta {
                k += kappa;
                round(
                    &mut buf,
                    delta,
                    p2,
                    one,
                    distance.wrapping_mul(if -kappa < 9 {
                        10u64.pow((-kappa) as u32)
                    } else {
                        0
                    }),
                );
                break;
            }
        }
    }
    let length = buf.len() as i32;
    let kk = length + k;
    if length <= kk && kk <= 21 {
        buf.resize(kk as usize, b'0');
    } else if 0 < kk && kk <= 21 {
        buf.insert(kk as usize, b'.');
    } else if -6 < kk && kk <= 0 {
        let mut prefix = vec![b'0'; (2 - kk) as usize];
        prefix[1] = b'.';
        prefix.extend(buf);
        buf = prefix;
    } else {
        if length > 1 {
            buf.insert(1, b'.');
        }
        buf.extend_from_slice(format!("e{:+}", kk - 1).as_bytes());
    }
    if v < 0. {
        buf.insert(0, b'-');
    }
    String::from_utf8(buf).unwrap()
}
