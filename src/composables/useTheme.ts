import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import type { ThemePreference } from "../types/client";

const STORAGE_KEY = "pmman.appearance";

export function useTheme() {
  const preference = ref<ThemePreference>("system");
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const systemDark = ref(media.matches);
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === "system" || saved === "light" || saved === "dark")
      preference.value = saved;
  } catch {
    /* A storage restriction must not prevent opening the client. */
  }
  const dark = computed(
    () =>
      preference.value === "dark" ||
      (preference.value === "system" && systemDark.value),
  );
  watch(
    dark,
    (value) => {
      document.documentElement.dataset.theme = value ? "dark" : "light";
    },
    { immediate: true },
  );
  watch(preference, (value) => {
    try {
      localStorage.setItem(STORAGE_KEY, value);
    } catch {
      /* Keep the session preference. */
    }
  });
  function changed(event: MediaQueryListEvent) {
    systemDark.value = event.matches;
  }
  onMounted(() => media.addEventListener("change", changed));
  onUnmounted(() => media.removeEventListener("change", changed));
  return { preference, dark };
}
