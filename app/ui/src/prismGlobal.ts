import { Prism } from "prism-react-renderer";

// prismjs component grammars are plain scripts that assign into the global `Prism`. The copy
// vendored inside prism-react-renderer never installs itself as a global, so this module does.
// It must be imported BEFORE any "prismjs/components/*" import (ESM evaluates sibling imports
// in source order) — prismLangs.ts below relies on that.
(globalThis as unknown as { Prism: unknown }).Prism = Prism;
