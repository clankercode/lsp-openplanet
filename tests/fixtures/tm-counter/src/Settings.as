[Setting name="Key: Increment Counter"]
VirtualKey S_IncrCounter = VirtualKey::Y;

[Setting name="Key: Decrement Counter"]
VirtualKey S_DecrCounter = VirtualKey::U;

[Setting name="Key: Reset Counter"]
VirtualKey S_ResetCounter = VirtualKey::K;

[Setting name="Counter Font Size" min=8 max=256]
float S_FontSize = 70.;

[Setting name="Show Updated Counter for (seconds)" description="0 for always" min=0 max=20]
float S_ShowForSecs = 2.5;

[Setting name="Increment/Decrement by" min=0 max=10]
int S_CounterIncrAmt = 1;

[Setting name="Screen Position (%)" drag min=0 max=100]
vec2 S_CounterPos = vec2(50, 15);

[Setting name="Font Color" color]
vec4 S_FontColor = vec4(1);

[Setting name="Stroke Color" color]
vec4 S_StrokeColor = vec4(0,0,0,1);

[SettingsTab name="State"]
void Render_S_State() {
    g_Counter = UI::InputInt("Current Counter", g_Counter);
}
