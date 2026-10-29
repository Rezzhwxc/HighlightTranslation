import { listen } from '@tauri-apps/api/event';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { getCurrentWindow } from '@tauri-apps/api/window';

const sourceEl = document.getElementById('source');
const translatedEl = document.getElementById('translated');
const copyBtn = document.getElementById('copy');
const win = getCurrentWindow();

// Скрыть оверлей (окно не уничтожается — его снова покажет хоткей)
async function hideOverlay() {
  try {
    await win.hide();
  } catch (err) {
    console.error('Не удалось скрыть окно:', err);
  }
}

async function copyAndClose() {
  try {
    await writeText(translatedEl.value); // берём из textarea — пользователь мог поправить текст
  } catch (err) {
    console.error('Не удалось записать в буфер:', err);
  }
  await hideOverlay();
}

copyBtn.addEventListener('click', copyAndClose);
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') hideOverlay();
});

async function init() {
  await listen('translation-result', (event) => {
    const { source, translated } = event.payload;
    sourceEl.value = source;
    translatedEl.value = translated;
    copyBtn.focus();
  });
}
init();
