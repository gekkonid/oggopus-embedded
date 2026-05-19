#include <opus.h>

/**
 * Struct big enough to contain OpusEncoder of opus-1.5.2 so it can be reserved
 * on stack.
 * <div rustbindgen replaces="OpusEncoder"></div>
 */
struct OpusEncoder {
#ifdef OPUS_EMBEDDED_SYS_STEREO
    char _unused[29356] __attribute__((aligned(4)));
#else
    char _unused[24612] __attribute__((aligned(4)));
#endif
};
