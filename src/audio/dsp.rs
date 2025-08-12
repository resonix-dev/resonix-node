#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}
impl Biquad {
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
    pub fn peaking(fs: f32, f0: f32, q: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / fs;
        let alpha = w0.sin() / (2.0 * q);
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * w0.cos();
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * w0.cos();
        let a2 = 1.0 - alpha / a;
        Self::norm(b0, b1, b2, a0, a1, a2)
    }
    pub fn low_shelf(fs: f32, f0: f32, s: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / fs;
        let alpha = w0.sin() / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
        let cosw = w0.cos();
        let sqrt_a = a.sqrt();
        let b0 = a * ((a + 1.0) - (a - 1.0) * cosw + 2.0 * sqrt_a * alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cosw);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cosw - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cosw + 2.0 * sqrt_a * alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cosw);
        let a2 = (a + 1.0) + (a - 1.0) * cosw - 2.0 * sqrt_a * alpha;
        Self::norm(b0, b1, b2, a0, a1, a2)
    }
    pub fn high_shelf(fs: f32, f0: f32, s: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / fs;
        let alpha = w0.sin() / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
        let cosw = w0.cos();
        let sqrt_a = a.sqrt();
        let b0 = a * ((a + 1.0) + (a - 1.0) * cosw + 2.0 * sqrt_a * alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cosw);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cosw - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cosw + 2.0 * sqrt_a * alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cosw);
        let a2 = (a + 1.0) - (a - 1.0) * cosw - 2.0 * sqrt_a * alpha;
        Self::norm(b0, b1, b2, a0, a1, a2)
    }
    fn norm(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z1: 0.0, z2: 0.0 }
    }
}

#[derive(Debug, Clone)]
pub struct Filters {
    pub volume: f32,
    pub eq: [f32; 5],
    pub eq_filters_l: [Biquad; 5],
    pub eq_filters_r: [Biquad; 5],
}
impl Default for Filters {
    fn default() -> Self {
        Self { volume: 1.0, eq: [0.0; 5], eq_filters_l: [Biquad::default(); 5], eq_filters_r: [Biquad::default(); 5] }
    }
}

pub fn update_eq_filters(filters: &mut Filters) {
    const FS: f32 = 48_000.0;
    let freqs = [60.0, 230.0, 910.0, 3600.0, 14_000.0];
    for (i, &f0) in freqs.iter().enumerate() {
        let gain = filters.eq[i];
        let q = if i == 0 || i == 4 { 0.707 } else { 1.0 };
        let b = if i == 0 { Biquad::low_shelf(FS, f0, q, gain) } else if i == 4 { Biquad::high_shelf(FS, f0, q, gain) } else { Biquad::peaking(FS, f0, q, gain) };
        filters.eq_filters_l[i] = b;
        filters.eq_filters_r[i] = b;
    }
}

pub fn biquad_eq_in_place(l: &mut [f32], r: &mut [f32], filters: &mut Filters) {
    for i in 0..l.len() {
        let mut xl = l[i];
        let mut xr = r[i];
        for j in 0..5 {
            xl = filters.eq_filters_l[j].process(xl);
            xr = filters.eq_filters_r[j].process(xr);
        }
        l[i] = xl;
        r[i] = xr;
    }
}
