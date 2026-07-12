// Prepared for the Chromium fork (enable_mersey). Wraps the Mersey C ABI
// (crates/mersey_capi/include/mersey.h) — the boundary proven by
// native/host_demo.c. Blink-facing surface kept deliberately identical to
// that demo so the integration risk is in wiring, not semantics.
#ifndef COMPONENTS_MERSEY_MERSEY_CONTEXT_H_
#define COMPONENTS_MERSEY_MERSEY_CONTEXT_H_

#include <cstdint>
#include <functional>
#include <string>
#include <string_view>

#include "mersey.h"  // C ABI header

namespace mersey {

// One engine context per Blink ExecutionContext (Document/origin).
class MerseyContext {
 public:
  struct HostCallbacks {
    std::function<void(std::string_view)> print;
    std::function<void(std::string_view)> error;
    std::function<void(std::string_view id, std::string_view text)> set_text;
    std::function<std::string(std::string_view id)> get_text;
    std::function<void(std::string_view id, uint32_t cb)> on_click;
  };

  explicit MerseyContext(HostCallbacks callbacks);
  ~MerseyContext();
  MerseyContext(const MerseyContext&) = delete;
  MerseyContext& operator=(const MerseyContext&) = delete;

  // 0 ok / 1 diagnostics / 2 runtime error (details via callbacks.error).
  uint32_t Run(std::string_view source);
  // Fire a registered event callback (posted from Blink task runners).
  uint32_t Invoke(uint32_t cb);

 private:
  HostCallbacks callbacks_;
  std::string get_text_scratch_;
  msy_context* ctx_ = nullptr;
};

}  // namespace mersey

#endif  // COMPONENTS_MERSEY_MERSEY_CONTEXT_H_
