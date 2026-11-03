// Typed `.wgsl` imports: the @vgpu/wgsl loader turns each into a prepared ShaderSource.
declare module "*.wgsl" {
  import type { ShaderSource } from "vgpu";
  const source: ShaderSource;
  export default source;
}
