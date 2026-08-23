// GH #58: a call to a namespace-qualified symbol that exists nowhere in the
// workspace is a game compile error the LSP used to miss:
//   Export_Test.as (38, 21) : ERR : No matching symbol 'TournamentStation::SendEvent'
// The namespace is declared in a SIBLING file (src/Station.as), so this also
// covers the cross-file half of the rule.
void Main() {
    TournamentStation::SendEvent("split", "{}"); // 7: ERROR — no near name
    TournamentStation::Known();                  // 8: clean — function
    TournamentStation::Widget w;                 // 9: clean — class as a type
    w.Value = TournamentStation::Counter;        // 10: clean — global variable
    TournamentStation::Widget();                 // 11: clean — value-type ctor
    TournamentStation::Mode(0);                  // 12: clean — enum cast
    TournamentStation::OnEvent();                // 13: clean — funcdef global
    Hints::Refrsh();                             // 14: ERROR — hint Hints::Refresh
    Hints::Refresh();                            // 15: clean
}
