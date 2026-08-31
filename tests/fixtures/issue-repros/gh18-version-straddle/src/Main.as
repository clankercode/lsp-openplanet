// GH #18: code migrated for Openplanet 1.29.14. `Text::Join` exists there
// but not on 1.29.5 (where the same helper was `string::Join`) — the
// version straddle a multi-version check must surface per target.
void Main() {
    print(Text::Join({"a", "b"}, ","));
}
