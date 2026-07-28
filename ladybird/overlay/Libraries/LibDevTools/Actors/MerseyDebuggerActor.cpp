/*
 * Copyright (c) 2026, Kirk D. Brown
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/JsonArray.h>
#include <AK/JsonObject.h>
#include <LibDevTools/Actors/MerseyDebuggerActor.h>
#include <LibDevTools/Actors/TabActor.h>
#include <LibDevTools/DevToolsDelegate.h>
#include <LibDevTools/DevToolsServer.h>

namespace DevTools {

NonnullRefPtr<MerseyDebuggerActor> MerseyDebuggerActor::create(DevToolsServer& devtools, String name, WeakPtr<TabActor> tab)
{
    return adopt_ref(*new MerseyDebuggerActor(devtools, move(name), move(tab)));
}

MerseyDebuggerActor::MerseyDebuggerActor(DevToolsServer& devtools, String name, WeakPtr<TabActor> tab)
    : Actor(devtools, move(name))
    , m_tab(move(tab))
{
    if (auto strong_tab = m_tab.strong_ref()) {
        devtools.delegate().listen_for_mersey_pause(
            strong_tab->description(),
            weak_callback(*this, [](auto& self, String snapshot) {
                self.on_paused(move(snapshot));
            }));
    }
}

MerseyDebuggerActor::~MerseyDebuggerActor() = default;

// action codes shared with Web::PageClient::mersey_debug_pause: 0=resume,
// 1=stepOver, 2=stepIn, 3=stepOut.
void MerseyDebuggerActor::handle_message(Message const& message)
{
    JsonObject response;

    if (message.type == "setBreakpoint"sv) {
        auto source = get_required_parameter<String>(message, "source"sv);
        if (!source.has_value())
            return;

        Vector<u32> lines;
        if (auto lines_array = message.data.get_array("lines"sv); lines_array.has_value()) {
            for (auto const& value : lines_array->values()) {
                if (auto line = value.get_integer<u32>(); line.has_value())
                    lines.append(*line);
            }
        }

        if (auto tab = m_tab.strong_ref())
            devtools().delegate().mersey_debug_set_breakpoints(tab->description(), *source, lines);

        send_response(message, move(response));
        return;
    }

    auto action = [&]() -> Optional<u8> {
        if (message.type == "resume"sv)
            return 0;
        if (message.type == "stepOver"sv)
            return 1;
        if (message.type == "stepIn"sv)
            return 2;
        if (message.type == "stepOut"sv)
            return 3;
        return {};
    }();

    if (action.has_value()) {
        if (auto tab = m_tab.strong_ref())
            devtools().delegate().mersey_debug_resume(tab->description(), *action);
        send_response(message, move(response));
        return;
    }

    send_unrecognized_packet_type_error(message);
}

void MerseyDebuggerActor::on_paused(String snapshot)
{
    JsonObject message;
    message.set("type"sv, "merseyPaused"sv);
    message.set("snapshot"sv, move(snapshot));
    send_message(move(message));
}

}
