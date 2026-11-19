import { create } from "zustand";
import { persist } from "zustand/middleware";

const LEFT_MIN = 190;
const LEFT_MAX = 520;
const RIGHT_MIN = 260;
const RIGHT_MAX = 620;

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

interface UiState {
  leftWidth: number;
  rightWidth: number;
  leftCollapsed: boolean;
  rightCollapsed: boolean;
  settingsOpen: boolean;
  newProjectOpen: boolean;
  styleStatsOpen: boolean;

  setLeftWidth: (width: number) => void;
  setRightWidth: (width: number) => void;
  toggleLeft: () => void;
  toggleRight: () => void;
  setSettingsOpen: (open: boolean) => void;
  setNewProjectOpen: (open: boolean) => void;
  setStyleStatsOpen: (open: boolean) => void;
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      leftWidth: 264,
      rightWidth: 340,
      leftCollapsed: false,
      rightCollapsed: false,
      settingsOpen: false,
      newProjectOpen: false,
      styleStatsOpen: false,

      setLeftWidth: (width) => set({ leftWidth: clamp(width, LEFT_MIN, LEFT_MAX) }),
      setRightWidth: (width) => set({ rightWidth: clamp(width, RIGHT_MIN, RIGHT_MAX) }),
      toggleLeft: () => set((s) => ({ leftCollapsed: !s.leftCollapsed })),
      toggleRight: () => set((s) => ({ rightCollapsed: !s.rightCollapsed })),
      setSettingsOpen: (open) => set({ settingsOpen: open }),
      setNewProjectOpen: (open) => set({ newProjectOpen: open }),
      setStyleStatsOpen: (open) => set({ styleStatsOpen: open }),
    }),
    {
      name: "storydesk.ui",
      // 只持久化布局偏好，弹窗状态不落盘
      partialize: (s) => ({
        leftWidth: s.leftWidth,
        rightWidth: s.rightWidth,
        leftCollapsed: s.leftCollapsed,
        rightCollapsed: s.rightCollapsed,
      }),
    },
  ),
);
