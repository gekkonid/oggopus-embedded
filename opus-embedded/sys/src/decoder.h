#include <opus.h>

/**
 * Struct big enough to contain OpusDecoder of opus-1.5.2 so it can be reserved
 * on stack.
 * <div rustbindgen replaces="OpusDecoder"></div>
 */
struct OpusDecoder {
    // aligned(16) yields better performance than aligned(4), at least on xtensa
#ifdef OPUS_EMBEDDED_SYS_STEREO
    char _unused[26580] __attribute__((aligned(16)));
#else
    char _unused[17860] __attribute__((aligned(16)));
#endif
};
