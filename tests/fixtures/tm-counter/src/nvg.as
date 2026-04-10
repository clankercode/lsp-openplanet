
int g_nvgFont = nvg::LoadFont("DroidSans-Bold.ttf");
const float TAU = 6.283185307179586;

void DrawNvgText(const string &in toDraw, const vec4 &in bufColor, bool isSecondary = false) {
    auto screen = vec2(Draw::GetWidth(), Draw::GetHeight());
    vec2 pos = (screen * S_CounterPos / 100.);
    float fontSize = S_FontSize;
    float sw = fontSize * 0.11;

    nvg::FontFace(g_nvgFont);
    nvg::FontSize(fontSize);
    nvg::TextAlign(nvg::Align::Left | nvg::Align::Middle);
    // auto sizeWPad = nvg::TextBounds(toDraw.SubStr(0, toDraw.Length - 3) + "000") + vec2(20, 10);

    if (isSecondary) {
        float secTimerScale = .65;
        pos = pos + vec2(0, fontSize * (1. + secTimerScale) / 2. + 10 - .25);
        fontSize *= secTimerScale;
        sw *= Math::Sqrt(secTimerScale);
        nvg::FontSize(fontSize);
    }

    // "stroke"
    if (true) {
        float nCopies = 32; // this does not seem to be expensive
        nvg::FillColor(S_StrokeColor);
        for (float i = 0; i < nCopies; i++) {
            float angle = TAU * float(i) / nCopies;
            vec2 offs = vec2(Math::Sin(angle), Math::Cos(angle)) * sw;
            nvg::Text(pos + offs, toDraw);
        }
    }

    nvg::FillColor(bufColor);
    nvg::Text(pos, toDraw);
}


void DrawNvgTitle(const string &in toDraw, const vec4 &in bufColor = vec4(1, 1, 1, 1)) {
    auto screen = vec2(Draw::GetWidth(), Draw::GetHeight());
    vec2 pos = (screen * vec2(0.5, 0.05));
    float fontSize = screen.y * 0.04;

    nvg::FontFace(g_nvgFont);
    nvg::FontSize(fontSize);
    nvg::TextAlign(nvg::Align::Center | nvg::Align::Middle);

    nvg::FillColor(bufColor);
    nvg::Text(pos, toDraw);
}
