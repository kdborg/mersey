/*
 * Copyright (c) 2026, Kirk D. Brown
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/NonnullRefPtr.h>
#include <LibDevTools/Actor.h>
#include <LibDevTools/Forward.h>

namespace DevTools {

// The interactive Mersey debugger (server half). Arms breakpoints and drives
// resume/step on the engine debug controller wired into MerseyScriptRunner, and
// emits a `merseyPaused` event carrying the pause snapshot when the engine stops.
class DEVTOOLS_API MerseyDebuggerActor final : public Actor {
public:
    static constexpr auto base_name = "mersey-debugger"sv;

    static NonnullRefPtr<MerseyDebuggerActor> create(DevToolsServer&, String name, WeakPtr<TabActor>);
    virtual ~MerseyDebuggerActor() override;

private:
    MerseyDebuggerActor(DevToolsServer&, String name, WeakPtr<TabActor>);

    virtual void handle_message(Message const&) override;

    void on_paused(String snapshot);

    WeakPtr<TabActor> m_tab;
};

}
