- When fixing bugs, always use TDD principles + red/green tests.
- When adding new test cases, try to generalize the case and create other variants of it. For example, if we needed to test a `const T &in` function argument, we shoudl also have tests for `&out` and non-const and `T@` and `T@ &in` etc.

