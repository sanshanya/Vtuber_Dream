/** cytoscape-fcose 无官方声明文件（包是纯 js 插件）——就地最小声明：
 *  断面 = cytoscape 插件扩展函数（注册即布局生效，消费面零导出）。 */
declare module "cytoscape-fcose" {
  import type { Ext } from "cytoscape";
  const extension: Ext;
  export default extension;
}
