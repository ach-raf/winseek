import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react({
    // React Compiler: automatic memoization (the manual useCallback/useMemo web stays but stops
    // being load-bearing). target 19 = no runtime helpers package needed on React 19.
    babel: { plugins: [["babel-plugin-react-compiler", { target: "19" }]] },
  })],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
});
