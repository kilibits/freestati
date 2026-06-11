async function init(): Promise<void> {
  const [version, platform] = await Promise.all([
    window.electron.getVersion(),
    window.electron.getPlatform(),
  ]);

  setText('app-version', `v${version}`);
  setText('platform', platform);

  const banner = document.getElementById('update-banner')!;
  const message = document.getElementById('update-message')!;
  const installBtn = document.getElementById('install-update') as HTMLButtonElement;

  window.electron.updater.onUpdateAvailable(() => {
    message.textContent = 'A new update is downloading…';
    banner.classList.remove('hidden');
  });

  window.electron.updater.onUpdateDownloaded(() => {
    message.textContent = 'Update ready.';
    installBtn.classList.remove('hidden');
  });

  installBtn.addEventListener('click', () => window.electron.updater.installUpdate());
}

function setText(id: string, value: string): void {
  const el = document.getElementById(id);
  if (el) el.textContent = value;
}

init();
