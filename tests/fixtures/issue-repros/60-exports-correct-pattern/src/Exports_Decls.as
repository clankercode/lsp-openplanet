// CORRECT pattern: the strict export file carries ONLY import
// declarations pointing at the defining module — these compile into
// consumers. Bodies live in ordinary source files.
import string SendEvent(string a, string b) from "T";
