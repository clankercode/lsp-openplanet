// B061: a read written in property syntax must credit the accessor that
// implements it. The CROSSED PAIR below is the load-bearing part: the same
// short name appears in two namespaces with OPPOSITE expected outcomes in
// BOTH directions, so anything keyed on the bare name gets one of them wrong
// no matter which way it guesses. Only the qualified form can split them.
//
//   Directives::Showing      IS read  -> Directives::get_Showing credited
//   Messages::Showing        NOT read -> Messages::get_Showing still warns
//   Messages::PendingCount   IS read  -> Messages::get_PendingCount credited
//   Directives::PendingCount NOT read -> Directives::get_PendingCount warns

namespace Directives {
    bool showing = false;
    bool get_Showing() { return showing; }          // credited: read as Directives::Showing

    int pendingCount = 0;
    int get_PendingCount() { return pendingCount; } // MUST STILL WARN: never read

    int historyCount = 0;
    int get_HistoryCount() { return historyCount; } // credited: read as Directives::HistoryCount
}

namespace Messages {
    bool showing = false;
    bool get_Showing() { return showing; }          // MUST STILL WARN: never read

    int pendingCount = 0;
    int get_PendingCount() { return pendingCount; } // credited: read as Messages::PendingCount
}

namespace Session {
    int status = 0;
    int get_Status() { return status; }             // credited: RMW reads
    void set_Status(int v) { status = v; }          // credited: qualified WRITE and RMW
}

namespace Quiet {
    int slot = 0;
    int get_Slot() { return slot; }                 // MUST STILL WARN: never read
    void set_Slot(int v) { slot = v; }              // MUST STILL WARN: never written
}
