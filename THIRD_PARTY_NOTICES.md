# Third-party notices

## Windows Settings catalog

`app/src-tauri/resources/windows-settings.json` is Flow Launcher's WindowsSettings list
(`Plugins/Flow.Launcher.Plugin.WindowsSettings/WindowsSettings.json`, dev branch), which itself
originates from Microsoft PowerToys Run. Both projects are MIT licensed:

- Flow Launcher — https://github.com/Flow-Launcher/Flow.Launcher (MIT)
- Microsoft PowerToys — https://github.com/microsoft/PowerToys (MIT)

The JSON file is committed unchanged (apart from being embedded verbatim) so future updates
from upstream stay a plain diff. The launcher's match/scoring algorithm is a Rust port of
Flow Launcher's `Flow.Launcher.Infrastructure/StringMatcher.cs` with the small documented
divergences listed in `app/src-tauri/src/launch/matcher.rs`.

MIT License (both projects):

Permission is hereby granted, free of charge, to any person obtaining a copy of this software
and associated documentation files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
