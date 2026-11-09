// The C surface secantus-icu calls: exactly the icu::Collator calls mongod
// makes (src/mongo/db/query/collation/collator_factory_icu.cpp and
// collator_interface_icu.cpp, r8.2.11), behind extern "C" so Rust needs no
// knowledge of ICU's C++ ABI or its versioned symbol names.

#include <cstring>
#include <memory>

#include "unicode/coll.h"
#include "unicode/locid.h"
#include "unicode/sortkey.h"
#include "unicode/stringpiece.h"
#include "unicode/ucol.h"
#include "unicode/udata.h"
#include "unicode/unistr.h"

extern "C" {

int32_t secantus_icu_set_common_data(const void* data) {
    UErrorCode status = U_ZERO_ERROR;
    udata_setCommonData(data, &status);
    return status;
}

// `icu::Locale::createFromName` then `icu::Collator::createInstance`, as
// mongod's makeFromBSON. On success `*out` owns the collator and `name` /
// `valid` hold `userLocale.getName()` and the collator's ULOC_VALID_LOCALE.
// `*bogus` is set when the locale itself does not parse.
int32_t secantus_icu_open(const char* locale, void** out, int32_t* bogus, char* name,
                          int32_t name_cap, char* valid, int32_t valid_cap) {
    *out = nullptr;
    auto user = icu::Locale::createFromName(locale);
    *bogus = user.isBogus() ? 1 : 0;
    if (*bogus) {
        return U_ZERO_ERROR;
    }
    UErrorCode status = U_ZERO_ERROR;
    std::unique_ptr<icu::Collator> c(icu::Collator::createInstance(user, status));
    if (U_FAILURE(status)) {
        return status;
    }
    icu::Locale got = c->getLocale(ULOC_VALID_LOCALE, status);
    if (U_FAILURE(status)) {
        return status;
    }
    std::strncpy(name, user.getName(), name_cap - 1);
    name[name_cap - 1] = '\0';
    std::strncpy(valid, got.getName(), valid_cap - 1);
    valid[valid_cap - 1] = '\0';
    *out = c.release();
    return U_ZERO_ERROR;
}

void secantus_icu_close(void* c) {
    delete static_cast<icu::Collator*>(c);
}

int32_t secantus_icu_get_attribute(void* c, int32_t attr, int32_t* value) {
    UErrorCode status = U_ZERO_ERROR;
    *value = static_cast<icu::Collator*>(c)->getAttribute(static_cast<UColAttribute>(attr),
                                                          status);
    return status;
}

int32_t secantus_icu_set_attribute(void* c, int32_t attr, int32_t value) {
    UErrorCode status = U_ZERO_ERROR;
    static_cast<icu::Collator*>(c)->setAttribute(static_cast<UColAttribute>(attr),
                                                 static_cast<UColAttributeValue>(value), status);
    return status;
}

int32_t secantus_icu_get_max_variable(void* c) {
    return static_cast<icu::Collator*>(c)->getMaxVariable();
}

int32_t secantus_icu_set_max_variable(void* c, int32_t group) {
    UErrorCode status = U_ZERO_ERROR;
    static_cast<icu::Collator*>(c)->setMaxVariable(static_cast<UColReorderCode>(group), status);
    return status;
}

// `compareUTF8`: -1, 0 or 1; `*status` is ICU's error code (only an
// allocation failure, mongod's fassert 34438).
int32_t secantus_icu_compare(const void* c, const char* a, int32_t alen, const char* b,
                             int32_t blen, int32_t* status) {
    UErrorCode s = U_ZERO_ERROR;
    UCollationResult r = static_cast<const icu::Collator*>(c)->compareUTF8(
        icu::StringPiece(a, alen), icu::StringPiece(b, blen), s);
    *status = s;
    return r;
}

// `getCollationKey(UnicodeString::fromUTF8(s))` as mongod's
// getComparisonKey: the key's bytes WITHOUT ICU's trailing NUL, copied into
// `out` when they fit in `cap`. Returns the length, or -1 with `*status` set.
int32_t secantus_icu_sort_key(const void* c, const char* s, int32_t len, uint8_t* out,
                              int32_t cap, int32_t* status) {
    UErrorCode st = U_ZERO_ERROR;
    icu::CollationKey key;
    static_cast<const icu::Collator*>(c)->getCollationKey(
        icu::UnicodeString::fromUTF8(icu::StringPiece(s, len)), key, st);
    *status = st;
    if (U_FAILURE(st)) {
        return -1;
    }
    int32_t n = 0;
    const uint8_t* bytes = key.getByteArray(n);
    if (n <= 0 || bytes == nullptr || bytes[n - 1] != 0) {
        *status = U_INTERNAL_PROGRAM_ERROR;
        return -1;
    }
    n -= 1;
    if (n <= cap) {
        std::memcpy(out, bytes, n);
    }
    return n;
}

}  // extern "C"
