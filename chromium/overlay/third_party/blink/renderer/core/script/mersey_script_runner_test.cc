// Mersey fork. Use of this source code is governed by the Chromium BSD-style
// license and the Mersey project license.

#include "third_party/blink/renderer/core/script/mersey_script_runner.h"

#include "testing/gtest/include/gtest/gtest.h"
#include "third_party/blink/renderer/core/dom/document.h"
#include "third_party/blink/renderer/core/dom/element.h"
#include "third_party/blink/renderer/core/testing/page_test_base.h"
#include "third_party/blink/renderer/platform/testing/task_environment.h"

namespace blink {

class MerseyScriptRunnerTest : public PageTestBase {};

// The engine and the header agree about the boundary. An embedder that skipped
// this and installed a mismatched host table would be handing the engine
// function pointers in the wrong order.
TEST_F(MerseyScriptRunnerTest, AbiVersionMatches) {
  EXPECT_EQ(msy_abi_version(), static_cast<uint32_t>(MSY_ABI_VERSION));
}

// A Mersey script writes to a DOM element through the host table. This is the
// whole path in miniature: Blink hands source to the engine, the engine
// compiles and runs it (Tier 0 straight into Tier 1 if it gets hot), and the
// only way back out is the table Blink installed.
TEST_F(MerseyScriptRunnerTest, WritesToTheDom) {
  SetBodyContent("<div id='out'>before</div>");
  MerseyScriptRunner& runner = MerseyScriptRunner::From(GetDocument());

  // The same surface web/demo/app.mersey and native/host_demo.c use: when the
  // host does not expose a real `document` global, `browser:dom` falls back to
  // the hand-written element API, which lands on the dom_* hooks this runner
  // wires to Blink's Element.
  uint32_t status = runner.Run(String(R"MSY(
import { document } from "browser:dom";
import { Element } from "browser:dom";
const out = document.getElementById("out") as Element;
out.textContent = "written by mersey";
)MSY"));

  EXPECT_EQ(status, 0u);
  Element* out = GetDocument().getElementById(AtomicString("out"));
  ASSERT_TRUE(out);
  EXPECT_EQ(out->textContent(), "written by mersey");
}

// A type error is a diagnostic, not a crash and not a run: status 1, and
// nothing executed.
TEST_F(MerseyScriptRunnerTest, TypeErrorDoesNotRun) {
  SetBodyContent("<div id='out'>untouched</div>");
  MerseyScriptRunner& runner = MerseyScriptRunner::From(GetDocument());

  uint32_t status = runner.Run(String("let x: int32 = \"not a number\";"));

  EXPECT_EQ(status, 1u);
  Element* out = GetDocument().getElementById(AtomicString("out"));
  ASSERT_TRUE(out);
  EXPECT_EQ(out->textContent(), "untouched");
}

// One context per Document, and it is the same one on the second lookup —
// which is what makes a page's scripts share globals, as they must.
TEST_F(MerseyScriptRunnerTest, OneContextPerDocument) {
  MerseyScriptRunner& a = MerseyScriptRunner::From(GetDocument());
  MerseyScriptRunner& b = MerseyScriptRunner::From(GetDocument());
  EXPECT_EQ(&a, &b);
}

}  // namespace blink
