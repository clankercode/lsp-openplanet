// B181: mutable string plus bool changed is a valid overload. The flags
// parameter is optional after the changed output, and the callback remains
// optional after flags.
void Main() {
    string value = "x";
    bool changed = false;

    // Legal: changed output with omitted optional flags and callback.
    string a = UI::InputText("##a", value, changed);
    // Legal: changed output followed by the typed flags enum.
    string b = UI::InputText("##b", value, changed, UI::InputTextFlags::None);
    // Legal: all optional parameters, including the callback slot.
    string c = UI::InputText("##c", value, changed, UI::InputTextFlags::None, null);

    // Invalid: a bool value is not the changed &out parameter.
    string bad = UI::InputText("##bad", value, false);
    // Invalid: the same rejection must survive an explicit flags argument.
    string badFlags = UI::InputText("##bad-flags", value, false, UI::InputTextFlags::None);

    print(a + b + c + tostring(changed) + bad + badFlags);
}
