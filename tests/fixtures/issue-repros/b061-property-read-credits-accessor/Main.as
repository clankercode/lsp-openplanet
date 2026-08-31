// Controls for the forms that ALREADY work. They are pinned so the fix
// cannot regress them, and so a fix that over-marks is caught: if the answer
// were "credit every get_X whenever X appears anywhere", the four
// never-read accessors in Accessors.as would go silent too.

int g = 0;
int get_G() { return g; }           // credited: global read as `G`
void set_G(int v) { g = v; }        // credited: global write as `G`

int lonely = 0;
int get_Lonely() { return lonely; } // MUST STILL WARN: never read

class C {
    int m = 0;
    int get_M() { return m; }       // credited: member read as `c.M`
    void set_M(int v) { m = v; }    // credited: member write as `c.M`
    int get_Untouched() { return 0; } // MUST STILL WARN: never read
}

void Main() {
    bool shown = Directives::Showing;         // qualified READ
    int hist = Directives::HistoryCount;      // qualified READ
    int pend = Messages::PendingCount;        // qualified READ
    Session::Status = 3;                      // qualified WRITE
    Session::Status += 1;                     // read-modify-write: BOTH

    int gv = G;                               // global read  (control)
    G = gv + 1;                               // global write (control)

    C@ c = C();
    int mv = c.M;                             // member read  (control)
    c.M = mv + 1;                             // member write (control)

    Inner::UseFromInside();

    print("" + shown + hist + pend + gv + mv);
}
