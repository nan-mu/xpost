//! Thin inject points for span / PC-hit — storage lives in xpost `ext::perf`.
//!
//! Call-sites keep RAII [`Span`] and [`hit_here`]; under `xdp-observe` they
//! forward to [`EspRadioTrace`](xpost_type::EspRadioTrace) on the mounted prog.

#![allow(missing_docs)]

pub use xpost_type::ProfSite as Site;

/// Enter a timed span; drop to close it on the mounted trace prog.
pub struct Span {
    site: Site,
    #[cfg(feature = "xdp-observe")]
    armed: bool,
}

impl Span {
    #[inline]
    pub fn enter(site: Site) -> Self {
        #[cfg(feature = "xdp-observe")]
        {
            super::current().span_begin(site);
            Self { site, armed: true }
        }
        #[cfg(not(feature = "xdp-observe"))]
        {
            Self { site }
        }
    }
}

impl Drop for Span {
    #[inline]
    fn drop(&mut self) {
        #[cfg(feature = "xdp-observe")]
        if self.armed {
            super::current().span_end(self.site);
        }
        let _ = self.site;
    }
}

/// Sample the return address of the caller (coarse; not a full stack walk).
#[inline(never)]
pub fn hit_here() {
    #[cfg(feature = "xdp-observe")]
    {
        let ra: u32;
        unsafe {
            core::arch::asm!("mv {0}, ra", out(reg) ra, options(nostack, preserves_flags));
        }
        super::current().pc_hit(ra);
    }
}
