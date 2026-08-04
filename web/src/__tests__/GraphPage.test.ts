/** GraphPage cose 降档钉（ag5-F9/ag4-F9：分界与两档迭代数）。 */
import { describe, expect, it } from "vitest";

import { graphLayoutIterations } from "../pages/GraphPage";

describe("cose numIter 随规模降档", () => {
  it("分界 500：以下 1000（小图收敛优先），以上 400（大图响应优先）", () => {
    expect(graphLayoutIterations(0)).toBe(1000);
    expect(graphLayoutIterations(499)).toBe(1000);
    expect(graphLayoutIterations(500)).toBe(400);
    expect(graphLayoutIterations(50_000)).toBe(400);
  });
});
