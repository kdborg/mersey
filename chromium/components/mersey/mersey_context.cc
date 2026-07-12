// See mersey_context.h. Trampolines route the C host table onto the
// std::function callbacks; this file has no Blink dependencies so it can be
// unit-tested in the fork with a plain gtest target.
#include "mersey_context.h"

namespace mersey {
namespace {

MerseyContext::HostCallbacks* Callbacks(void* data);

void Print(void* data, const char* s, size_t len);
void Error(void* data, const char* s, size_t len);
void SetText(void* data, const char* id, size_t il, const char* t, size_t tl);
const char* GetText(void* data, const char* id, size_t il, size_t* out_len);
void OnClick(void* data, const char* id, size_t il, uint32_t cb);

}  // namespace

// Definitions live in the fork where MerseyContext is linked against the
// static library; kept here as the reviewed reference implementation.
//
// MerseyContext::MerseyContext(HostCallbacks callbacks)
//     : callbacks_(std::move(callbacks)) {
//   msy_host_table table = {this, &Print, &Error, &SetText, &GetText,
//                           &OnClick};
//   ctx_ = msy_context_new(&table);
// }
// ... (Run/Invoke forward to msy_context_run / msy_context_invoke)

}  // namespace mersey
