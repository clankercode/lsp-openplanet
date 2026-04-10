// void Notify(const string &in msg) {
//     UI::ShowNotification(Meta::ExecutingPlugin().Name, msg);
//     trace("Notified: " + msg);
// }

// void NotifyError(const string &in msg) {
//     warn(msg);
//     UI::ShowNotification(Meta::ExecutingPlugin().Name + ": Error", msg, vec4(.9, .3, .1, .3), 15000);
// }

// void NotifyWarning(const string &in msg) {
//     warn(msg);
//     UI::ShowNotification(Meta::ExecutingPlugin().Name + ": Warning", msg, vec4(.9, .6, .2, .3), 15000);
// }

const string PluginIcon = Icons::Calculator;
const string MenuTitle = "\\$38f" + PluginIcon + "\\$z " + Meta::ExecutingPlugin().Name;

// show the window immediately upon installation
[Setting hidden]
bool S_IsActive = true;

/** Render function called every frame intended only for menu items in `UI`. */
void RenderMenu() {
    if (UI::MenuItem(MenuTitle, "", S_IsActive)) {
        S_IsActive = !S_IsActive;
    }
}

/** Render function called every frame.
*/
void Render() {
    if (!S_IsActive) return;
    if (S_ShowForSecs > 0 && float(Time::Now - lastCounterUpdateTime) / 1000.0 > S_ShowForSecs) return;
    nvg::Reset();
    nvg::BeginPath();
    DrawNvgText(tostring(g_Counter), S_FontColor);
    nvg::ClosePath();
}

/** Called whenever a key is pressed on the keyboard. See the documentation for the [`VirtualKey` enum](https://openplanet.dev/docs/api/global/VirtualKey).
*/
UI::InputBlocking OnKeyPress(bool down, VirtualKey key) {
    if (!down) return UI::InputBlocking::DoNothing;

    if (key == S_IncrCounter) OnIncrCounter();
    else if (key == S_DecrCounter) OnDecrCounter();
    else if (key == S_ResetCounter) OnResetCounter();
    // else if (key == S_StopTimer) OnIncrCounter();
    // else if (key == S_StartTimer) OnIncrCounter();

    return UI::InputBlocking::DoNothing;
}


void OnCounterUpdate() {
    lastCounterUpdateTime = Time::Now;
}

uint lastCounterUpdateTime = 0;

[Setting hidden]
int g_Counter = 0;

void OnIncrCounter() {
    OnCounterUpdate();
    g_Counter += S_CounterIncrAmt;
}
void OnDecrCounter() {
    OnCounterUpdate();
    g_Counter -= S_CounterIncrAmt;
}
void OnResetCounter() {
    OnCounterUpdate();
    g_Counter = 0;
}
