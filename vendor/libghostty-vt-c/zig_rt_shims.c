/*
 * Minimal C99 hardware-float conversion shims for Zig 0.16 C backend output.
 *
 * zig.h lowers float<->int and float<->double conversions through compiler-rt
 * helper names (__truncdfsf2, __extendsfdf2, __floatunsidf, etc.). On Linux
 * targets with mandatory hardware IEEE-754 floating point (such as aarch64),
 * system libgcc omits soft-float helpers from libgcc.a. Providing weak hidden
 * C99 definitions resolves them to single hardware instructions without
 * conflicting if a toolchain runtime already defines them.
 */
#include <stdint.h>

#if defined(__GNUC__) || defined(__clang__)
#define ZIG_RT_SHIM __attribute__((weak, visibility("hidden")))
#else
#define ZIG_RT_SHIM
#endif

ZIG_RT_SHIM float __truncdfsf2(double a) { return (float)a; }
ZIG_RT_SHIM double __extendsfdf2(float a) { return (double)a; }

ZIG_RT_SHIM int32_t __fixsfsi(float a) { return (int32_t)a; }
ZIG_RT_SHIM int64_t __fixsfdi(float a) { return (int64_t)a; }
ZIG_RT_SHIM uint32_t __fixunssfsi(float a) { return a <= 0.0f ? 0U : (uint32_t)a; }
ZIG_RT_SHIM uint64_t __fixunssfdi(float a) { return a <= 0.0f ? 0ULL : (uint64_t)a; }

ZIG_RT_SHIM int32_t __fixdfsi(double a) { return (int32_t)a; }
ZIG_RT_SHIM int64_t __fixdfdi(double a) { return (int64_t)a; }
ZIG_RT_SHIM uint32_t __fixunsdfsi(double a) { return a <= 0.0 ? 0U : (uint32_t)a; }
ZIG_RT_SHIM uint64_t __fixunsdfdi(double a) { return a <= 0.0 ? 0ULL : (uint64_t)a; }

ZIG_RT_SHIM float __floatsisf(int32_t a) { return (float)a; }
ZIG_RT_SHIM float __floatdisf(int64_t a) { return (float)a; }
ZIG_RT_SHIM float __floatunsisf(uint32_t a) { return (float)a; }
ZIG_RT_SHIM float __floatundisf(uint64_t a) { return (float)a; }

ZIG_RT_SHIM double __floatsidf(int32_t a) { return (double)a; }
ZIG_RT_SHIM double __floatdidf(int64_t a) { return (double)a; }
ZIG_RT_SHIM double __floatunsidf(uint32_t a) { return (double)a; }
ZIG_RT_SHIM double __floatundidf(uint64_t a) { return (double)a; }
