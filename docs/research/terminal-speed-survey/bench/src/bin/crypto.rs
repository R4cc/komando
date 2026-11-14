//! Decrypt throughput of the AEADs russh uses, via ring, on SSH-sized packets.
use ring::aead::{self, chacha20_poly1305_openssh as cp};
use std::time::Instant;

fn main() {
    let total: usize = 512 << 20; // 512 MiB per cipher
    for &pkt in &[32 * 1024usize, 256 * 1024] {
        let n = total / pkt;
        // chacha20-poly1305@openssh.com
        let key = [7u8; cp::KEY_LEN];
        let seal = cp::SealingKey::new(&key);
        let open = cp::OpeningKey::new(&key);
        let mut buf = vec![0x41u8; 4 + pkt];
        buf[..4].copy_from_slice(&(pkt as u32).to_be_bytes());
        let mut tag = [0u8; cp::TAG_LEN];
        seal.seal_in_place(1, &mut buf, &mut tag);
        let sealed = buf.clone();
        let t = Instant::now();
        for _ in 0..n {
            buf.copy_from_slice(&sealed);
            open.open_in_place(1, &mut buf, &tag).unwrap();
        }
        let s = t.elapsed().as_secs_f64();
        println!("chacha20-poly1305 open, {:>3} KiB packets: {:7.0} MB/s", pkt / 1024, (n * pkt) as f64 / 1e6 / s);
        let t = Instant::now();
        for _ in 0..n { seal.seal_in_place(1, &mut buf, &mut tag); }
        let s = t.elapsed().as_secs_f64();
        println!("chacha20-poly1305 seal, {:>3} KiB packets: {:7.0} MB/s", pkt / 1024, (n * pkt) as f64 / 1e6 / s);

        for (name, alg) in [("aes128-gcm", &aead::AES_128_GCM), ("aes256-gcm", &aead::AES_256_GCM)] {
            let k = aead::LessSafeKey::new(aead::UnboundKey::new(alg, &vec![9u8; alg.key_len()]).unwrap());
            let mut data = vec![0x41u8; pkt];
            let nonce = || aead::Nonce::assume_unique_for_key([1u8; 12]);
            let tag = k.seal_in_place_separate_tag(nonce(), aead::Aad::from([0u8; 4]), &mut data).unwrap();
            let sealed = data.clone();
            let t = Instant::now();
            for _ in 0..n {
                data.copy_from_slice(&sealed);
                k.open_in_place_separate_tag(nonce(), aead::Aad::from([0u8; 4]), tag, &mut data, 0..).unwrap();
            }
            let s = t.elapsed().as_secs_f64();
            println!("{name:<17} open, {:>3} KiB packets: {:7.0} MB/s", pkt / 1024, (n * pkt) as f64 / 1e6 / s);
        }
        // memcpy reference (the copy each loop includes)
        let t = Instant::now();
        for _ in 0..n { buf.copy_from_slice(&sealed); std::hint::black_box(&buf); }
        let s = t.elapsed().as_secs_f64();
        println!("memcpy alone,      {:>3} KiB packets: {:7.0} MB/s", pkt / 1024, (n * pkt) as f64 / 1e6 / s);
    }
}
