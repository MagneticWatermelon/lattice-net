use std::time::Instant;
use std::hint::black_box;
#[derive(Clone, Copy)] struct Body { pos: [f32;2], vel:[f32;2], squad: u32, _pad: [u32; 2] } // 28 B like sim Body
fn rng(s: &mut u64) -> f32 { *s ^= *s << 13; *s ^= *s >> 7; *s ^= *s << 17; (*s >> 40) as f32 / (1u64<<24) as f32 }
fn main() {
    let k = 100usize;
    for &(n, rad) in &[(600usize, 150.0f32), (3000, 100.0), (10000, 12.5)] {
        let mut s = 0x9E3779B97F4A7C15u64;
        let bodies: Vec<Body> = (0..n).map(|_| Body{pos:[4000.0+rng(&mut s)*2.0*rad, 4000.0+rng(&mut s)*2.0*rad], vel:[0.0;2], squad:0, _pad:[0;2]}).collect();
        // ids in "cell order" = random permutation relative to body array (indirection)
        let mut ids: Vec<u32> = (0..n as u32).collect();
        for i in (1..n).rev() { let j = (rng(&mut s) * (i as f32)) as usize; ids.swap(i, j); }
        // SoA copy in item order (what a grid with inline positions would hold)
        let xs: Vec<f32> = ids.iter().map(|&i| bodies[i as usize].pos[0]).collect();
        let ys: Vec<f32> = ids.iter().map(|&i| bodies[i as usize].pos[1]).collect();
        let queries: Vec<[f32;2]> = (0..256).map(|q| bodies[q % n].pos).collect();
        let reps = (2_000_000 / n).max(20);
        let r2 = (rad*4.0).powi(2);
        let mut t = |name: &str, f: &mut dyn FnMut([f32;2]) -> usize| {
            let st = Instant::now(); let mut acc = 0;
            for r in 0..reps { acc += f(queries[r % queries.len()]); }
            let ns = st.elapsed().as_nanos() as f64 / reps as f64;
            println!("n={n:5} {name:44} {:9.0} ns/query {:6.2} ns/cand  (chk {})", ns, ns / n as f64, acc % 7);
        };
        let mut v3: Vec<(f32,f32,u16)> = Vec::with_capacity(n);
        t("A current: AoS gather + tuple + select_nth", &mut |p| { v3.clear();
            for &j in &ids { let b = &bodies[j as usize]; let d2 = (b.pos[0]-p[0]).powi(2)+(b.pos[1]-p[1]).powi(2); if d2 <= r2 { v3.push((d2,d2,j as u16)); } }
            if v3.len() > k { v3.select_nth_unstable_by(k-1, |a,b| a.0.total_cmp(&b.0)); v3.select_nth_unstable_by(k, |a,b| a.0.total_cmp(&b.0)); v3.truncate(k);} black_box(&v3); v3.len() });
        let mut dd: Vec<f32> = vec![0.0; n];
        t("dist only: AoS gather -> d2[]", &mut |p| { for (o,&j) in dd.iter_mut().zip(&ids) { let b=&bodies[j as usize]; *o=(b.pos[0]-p[0]).powi(2)+(b.pos[1]-p[1]).powi(2);} black_box(&dd); 1 });
        t("dist only: SoA contiguous -> d2[]", &mut |p| { for ((o,&x),&y) in dd.iter_mut().zip(&xs).zip(&ys) { *o=(x-p[0]).powi(2)+(y-p[1]).powi(2);} black_box(&dd); 1 });
        let mut v64: Vec<u64> = Vec::with_capacity(n);
        t("B SoA + packed u64 key + select_nth", &mut |p| { v64.clear();
            for (i,(&x,&y)) in xs.iter().zip(&ys).enumerate() { let d2=(x-p[0]).powi(2)+(y-p[1]).powi(2); if d2<=r2 { v64.push(((d2.to_bits() as u64)<<32) | ids[i] as u64);} }
            if v64.len()>k { v64.select_nth_unstable(k-1); v64.truncate(k);} black_box(&v64); v64.len() });
        let mut buf: Vec<u64> = Vec::with_capacity(4*k);
        t("C SoA + u64 key + threshold buf(4k)", &mut |p| { buf.clear(); let mut thr = r2.to_bits() as u64 * (1u64<<32) | 0xffff_ffff;
            for (i,(&x,&y)) in xs.iter().zip(&ys).enumerate() { let d2=(x-p[0]).powi(2)+(y-p[1]).powi(2); let key=((d2.to_bits() as u64)<<32)|ids[i] as u64;
                if key < thr { buf.push(key); if buf.len()==4*k { buf.select_nth_unstable(k-1); buf.truncate(k); thr = buf[k-1]; } } }
            if buf.len()>k { buf.select_nth_unstable(k-1); buf.truncate(k);} black_box(&buf); buf.len() });
        let mut keys: Vec<u64> = vec![0; n];
        t("C' SoA branchless keys[] then thr-filter", &mut |p| {
            for (o,(&x,&y)) in keys.iter_mut().zip(xs.iter().zip(&ys)) { let d2=(x-p[0]).powi(2)+(y-p[1]).powi(2); *o=(d2.to_bits() as u64)<<32; }
            for (o,&id) in keys.iter_mut().zip(&ids) { *o |= id as u64; }
            buf.clear(); let mut thr = (r2.to_bits() as u64)<<32 | 0xffff_ffff;
            for &key in &keys { if key<thr { buf.push(key); if buf.len()==4*k { buf.select_nth_unstable(k-1); buf.truncate(k); thr=buf[k-1]; } } }
            if buf.len()>k { buf.select_nth_unstable(k-1); buf.truncate(k);} black_box(&buf); buf.len() });

        let mut v32: Vec<u32> = Vec::with_capacity(n);
        let qs = 65535.0 / r2; // quantize d2 to u16 over [0, r2]
        t("E SoA + u32 key (u16 qd2|u16 id) + select", &mut |p| { v32.clear();
            for (i,(&x,&y)) in xs.iter().zip(&ys).enumerate() { let d2=(x-p[0]).powi(2)+(y-p[1]).powi(2); if d2<=r2 { v32.push((((d2*qs) as u32)<<16) | (ids[i] & 0xffff)); } }
            if v32.len()>k { v32.select_nth_unstable(k-1); v32.truncate(k);} black_box(&v32); v32.len() });
        let mut q16: Vec<u16> = vec![0; n];
        let mut out: Vec<u32> = Vec::with_capacity(2*k+64);
        t("F SoA + u16 qd2[] + 256-bin histogram select", &mut |p| {
            let mut h = [0u32; 256];
            for (o,(&x,&y)) in q16.iter_mut().zip(xs.iter().zip(&ys)) { let d2=(x-p[0]).powi(2)+(y-p[1]).powi(2); *o = (d2*qs).min(65535.0) as u16; }
            for &q in &q16 { h[(q>>8) as usize] += 1; }
            let (mut acc, mut b) = (0u32, 0usize); while b < 255 && acc + h[b] < k as u32 { acc += h[b]; b += 1; }
            let cut = ((b as u32 + 1) << 8) as u32; out.clear();
            for (i,&q) in q16.iter().enumerate() { if (q as u32) < cut { out.push(((q as u32)<<16)|(ids[i]&0xffff)); } }
            if out.len()>k { out.select_nth_unstable(k-1); out.truncate(k);} black_box(&out); out.len() });
        let mut heap = std::collections::BinaryHeap::<u64>::with_capacity(k+1);
        t("D SoA + BinaryHeap<u64> size k", &mut |p| { heap.clear();
            for (i,(&x,&y)) in xs.iter().zip(&ys).enumerate() { let d2=(x-p[0]).powi(2)+(y-p[1]).powi(2); if d2>r2 {continue;} let key=((d2.to_bits() as u64)<<32)|ids[i] as u64;
                if heap.len()<k { heap.push(key);} else if key < *heap.peek().unwrap() { *heap.peek_mut().unwrap() = key; } }
            black_box(&heap); heap.len() });
    }
}
