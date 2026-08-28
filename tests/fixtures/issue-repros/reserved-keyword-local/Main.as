// todo-cover-cases-next.txt, 2026-08-24, tm-tournament-station:
// game: `Instead found reserved keyword 'out'` (same for `in` / `inout`).
// These names are parameter-direction keywords and cannot be identifiers.

void Words() {
    string out;          // 6: ERROR reserved keyword 'out'
    out += "x";
}

void InLocal() {
    int in;              // 11: ERROR reserved keyword 'in'
    in = 1;
}

void InoutLocal() {
    float inout;         // 16: ERROR reserved keyword 'inout'
    inout = 1.0;
}

void AutoOut() {
    auto out = "x";      // 21: ERROR reserved keyword 'out'
}

void HandleOut() {
    dictionary@ out;     // 25: ERROR reserved keyword 'out'
}

class Holder {
    string out;          // 30: ERROR reserved keyword 'out'
    int in;              // 31: ERROR reserved keyword 'in'
    float inout;         // 32: ERROR reserved keyword 'inout'
}

// Legal: `in` / `out` / `inout` as parameter *modifiers*, not names.
void Legal(const string &in s, int &out n, float &inout x) {
    n = s.Length;
    x += 1.0;
}

void Main() {}
