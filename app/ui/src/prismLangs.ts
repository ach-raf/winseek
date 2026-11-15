// Languages the preview highlights that prism-react-renderer does not vendor. Each prismjs
// component file is a tiny grammar script registering itself on the global Prism that
// prismGlobal.ts installs (its import must stay first: ESM runs imports in source order).
// Everything here is a standalone grammar (scss/less build on the already-vendored css one).
import "./prismGlobal";
import "prismjs/components/prism-bash";
import "prismjs/components/prism-batch";
import "prismjs/components/prism-csharp";
import "prismjs/components/prism-diff";
import "prismjs/components/prism-ini";
import "prismjs/components/prism-java";
import "prismjs/components/prism-less";
import "prismjs/components/prism-powershell";
import "prismjs/components/prism-ruby";
import "prismjs/components/prism-scss";
import "prismjs/components/prism-toml";
