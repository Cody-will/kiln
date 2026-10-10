/* Floating-point math for std::math: thin wrappers over the C library, so
 * results match the platform libm (correctly rounded where libm is). */
#include <math.h>
#include <stdint.h>

double kn_math1(int32_t op, double x) {
    switch (op) {
    case 0: return sin(x);
    case 1: return cos(x);
    case 2: return tan(x);
    case 3: return asin(x);
    case 4: return acos(x);
    case 5: return atan(x);
    case 6: return sinh(x);
    case 7: return cosh(x);
    case 8: return tanh(x);
    case 9: return asinh(x);
    case 10: return acosh(x);
    case 11: return atanh(x);
    case 12: return exp(x);
    case 13: return exp2(x);
    case 14: return expm1(x);
    case 15: return log(x);
    case 16: return log2(x);
    case 17: return log10(x);
    case 18: return log1p(x);
    case 19: return cbrt(x);
    case 20: return trunc(x);
    case 21: return erf(x);
    case 22: return erfc(x);
    case 23: return tgamma(x);
    case 24: return lgamma(x);
    default: return NAN;
    }
}

double kn_math2(int32_t op, double x, double y) {
    switch (op) {
    case 0: return pow(x, y);
    case 1: return atan2(x, y);
    case 2: return hypot(x, y);
    case 3: return fmod(x, y);
    case 4: return copysign(x, y);
    case 5: return remainder(x, y);
    case 6: return nextafter(x, y);
    default: return NAN;
    }
}

double kn_math_fma(double x, double y, double z) { return fma(x, y, z); }
