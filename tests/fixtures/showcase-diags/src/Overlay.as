// NanoVG / const / handle — diverse type shapes.

void DrawOverlayFrame() {
    nvg::Reset();
    nvg::BeginPath();

    // FillColor wants vec4 — pass vec3
    nvg::FillColor(vec3(1.0f, 0.2f, 0.1f));

    nvg::ClosePath();
}

void DrawBadgeLayer() {
    // const violation
    const int layers = 2;
    layers = 3;

    // handle/value mismatch — @assign onto a value type
    int count = 0;
    @count = null;

    // type mismatch into local workspace call (MakeTint wants int)
    vec4 tint = MakeTint(true);
    nvg::FillColor(tint);
}

vec4 MakeTint(int intensity) {
    float a = float(intensity) / 255.0f;
    return vec4(1.0f, 1.0f, 1.0f, a);
}

// DEPENDENCY_* defined for optional_dependencies in info.toml.
#if DEPENDENCY_SHOWCASEFAKEVEHICLE
void DrawSpeedMaybe() {
    // unknown type under DEPENDENCY_SHOWCASEFAKEVEHICLE
    FakeVehicleState@ st;
    float speed = st.WorldVel;
}
#endif
