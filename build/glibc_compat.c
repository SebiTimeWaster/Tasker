/*
 * Keep the binary runnable on older glibc (glibc 2.34+: Linux Mint 21+, Ubuntu 22.04+, Debian 12+).
 *
 * Newer glibc releases add new *versions* of some libm functions (e.g. acosf@GLIBC_2.43),
 * and the linker always picks the newest one available on the build machine, so a binary
 * built on a new distro refuses to start on an older one. build.rs links with
 * `-Wl,--wrap=<fn>`, which redirects our calls to `__wrap_<fn>` below, and those call the
 * original x86_64 baseline version (GLIBC_2.2.5) that every glibc still provides.
 */
#if defined(__x86_64__) && defined(__linux__)

#define PIN_OLD(name, ret, params, args)                       \
    ret __old_##name params;                                   \
    __asm__(".symver __old_" #name "," #name "@GLIBC_2.2.5");  \
    ret __wrap_##name params { return __old_##name args; }

PIN_OLD(acosf, float, (float x), (x))
PIN_OLD(asinf, float, (float x), (x))
PIN_OLD(atan2f, float, (float y, float x), (y, x))
PIN_OLD(acoshf, float, (float x), (x))
PIN_OLD(coshf, float, (float x), (x))
PIN_OLD(sinhf, float, (float x), (x))
PIN_OLD(hypot, double, (double x, double y), (x, y))
PIN_OLD(hypotf, float, (float x, float y), (x, y))

#endif
