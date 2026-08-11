// Workspace helpers — warnings + call-site mismatches.

// bare `string` param → StringByValueParam warning (game sanity check)
void ShowStatus(string msg) {
    UI::Text(msg);
}

// another by-value string warning
string PrefixLabel(string raw) {
    return "[demo] " + raw;
}

// returns int — callers that treat it as string should mismatch
int ComputeTitle() {
    return 7;
}

class ScoreBoard {
    int points;
    const int cap;

    void Add(int n) {
        points += n;
    }

    void ResetWrong() {
        // const field write
        cap = 0;
        // undefined member on external string
        string name = "board";
        int bad = name.NotARealMethod();
    }
}

void TallyScores() {
    ScoreBoard@ board;
    board.Add(1);

    // arg count mismatch on workspace method (Add expects 1)
    board.Add(1, 2, 3);

    // invalid assignment target
    5 = board.points;

    // unknown type in array element position
    array<MysteryNod@>@ nods;

    // undefined identifier
    int total = g_SeasonBest;
}
