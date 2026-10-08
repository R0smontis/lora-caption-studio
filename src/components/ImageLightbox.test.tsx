import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { backend } from "../lib/backend";
import { ImageLightbox } from "./ImageLightbox";

vi.mock("../lib/backend", () => ({ backend: { getImagePreview: vi.fn() } }));

describe("ImageLightbox canvas viewer", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(backend.getImagePreview).mockResolvedValue("data:image/jpeg;base64,LARGE");
  });

  it("supports continuous wheel zoom, drag pan, rotation and fit reset", async () => {
    const { container } = render(<ImageLightbox path="D:\\images\\portrait.png" name="portrait.png" thumbnail="data:image/jpeg;base64,THUMB" onClose={vi.fn()} />);
    const image = await screen.findByRole("img", { name: "portrait.png" });
    Object.defineProperty(image, "naturalWidth", { configurable: true, value: 1200 });
    Object.defineProperty(image, "naturalHeight", { configurable: true, value: 800 });
    fireEvent.load(image);
    const stage = container.ownerDocument.querySelector(".image-lightbox-stage") as HTMLElement;
    Object.defineProperty(stage, "getBoundingClientRect", { configurable: true, value: () => ({ left: 0, top: 0, width: 1000, height: 700, right: 1000, bottom: 700, x: 0, y: 0, toJSON: () => ({}) }) });
    fireEvent.click(screen.getByRole("button", { name: "完整显示" }));
    const before = screen.getByLabelText("当前缩放比例").textContent;
    fireEvent.wheel(stage, { deltaY: -137, clientX: 600, clientY: 350 });
    await waitFor(() => expect(screen.getByLabelText("当前缩放比例").textContent).not.toBe(before));

    const pointer = (type: string, x: number, y: number) => {
      const event = new Event(type, { bubbles: true });
      Object.defineProperties(event, { pointerId: { value: 1 }, clientX: { value: x }, clientY: { value: y } });
      fireEvent(stage, event);
    };
    pointer("pointerdown", 500, 350);
    pointer("pointermove", 570, 390);
    pointer("pointerup", 570, 390);
    const canvas = container.ownerDocument.querySelector(".image-lightbox-canvas") as HTMLElement;
    expect(canvas).toHaveClass("ready");
    expect(canvas.style.transform).toMatch(/^translate3d\([1-9]\d*(?:\.\d+)?px, 40px, 0\)$/);

    fireEvent.click(screen.getByRole("button", { name: "向右旋转" }));
    expect(image.style.transform).toContain("rotate(90deg)");
    fireEvent.click(screen.getByRole("button", { name: "实际大小" }));
    expect(screen.getByLabelText("当前缩放比例")).toHaveTextContent("100%");
  });
});
