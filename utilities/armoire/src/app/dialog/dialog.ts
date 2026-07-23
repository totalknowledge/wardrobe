import { Component } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { DialogAction, DialogService } from './dialog-service';
import { WardrobeService } from '../wardrobe/wardrobe-service';

@Component({
  selector: 'app-dialog',
  imports: [FormsModule],
  templateUrl: './dialog.html',
  styleUrl: './dialog.scss',
})
export class DialogComponent {
  constructor(
    public dialogService: DialogService,
    private databaseService: WardrobeService,
  ) {}

  public get dialogTitle(): string {
    return this.dialogService.dialogs()?.title ?? '';
  }

  public get dialogBody(): string {
    return this.dialogService.dialogs()?.body ?? '';
  }

  public get dialogVersion(): string {
    return this.dialogService.dialogs()?.version ?? '';
  }

  public get dialogShowInput(): boolean {
    return this.dialogService.dialogs()?.showInput === true;
  }

  public get dialogInputPlaceholder(): string {
    return this.dialogService.dialogs()?.inputPlaceholder ?? '';
  }

  public get dialogActions(): DialogAction[] {
    return this.dialogService.dialogs()?.actions ?? [];
  }

  public get dialogHasActions(): boolean {
    return this.dialogActions.length > 0;
  }

  public get dialogHasConfirm(): boolean {
    return typeof this.dialogService.dialogs()?.onConfirm === 'function';
  }

  public get dialogConfirmText(): string {
    return this.dialogService.dialogs()?.confirmText ?? 'Confirm';
  }

  public get dialogCancelText(): string {
    return this.dialogService.dialogs()?.cancelText ?? 'Cancel';
  }

  public get showConnectionModal(): boolean {
    return this.dialogService.showConnectionModal();
  }

  public get connectionModalType(): 'location' | 'connection' {
    return this.dialogService.connectionModalType;
  }

  public get fileLocationPath(): string {
    return this.dialogService.fileLocationPath;
  }

  public set fileLocationPath(value: string) {
    this.dialogService.fileLocationPath = value;
  }

  public get createIfNotExist(): boolean {
    return this.dialogService.createIfNotExist;
  }

  public set createIfNotExist(value: boolean) {
    this.dialogService.createIfNotExist = value;
  }

  public get connectionUri(): string {
    return this.dialogService.connectionUri;
  }

  public set connectionUri(value: string) {
    this.dialogService.connectionUri = value;
  }

  public get connectionName(): string {
    return this.dialogService.connectionName;
  }

  public set connectionName(value: string) {
    this.dialogService.connectionName = value;
  }

  public get connectionError(): string | null {
    return this.dialogService.connectionError;
  }

  public get connectionTestMessage(): string | null {
    return this.dialogService.connectionTestMessage;
  }

  public get isConnecting(): boolean {
    return this.dialogService.isConnecting;
  }

  public get isTestingConnection(): boolean {
    return this.dialogService.isTestingConnection;
  }

  public closeConnectionModal(): void {
    this.dialogService.closeConnectionModal();
  }

  public onFolderSelected(event: Event): void {
    const input = event.target as HTMLInputElement;
    if (!input.files || input.files.length === 0) {
      return;
    }

    const file = input.files[0] as File & { path?: string };
    if (file.path) {
      const isWin = window.navigator.userAgent.includes('Windows');
      const sep = isWin ? '\\' : '/';
      const lastIdx = file.path.lastIndexOf(sep);
      this.dialogService.fileLocationPath = lastIdx !== -1 ? file.path.substring(0, lastIdx) : file.path;
    } else {
      this.dialogService.fileLocationPath = file.webkitRelativePath || file.name || '';
    }

    this.updateConnectionNameGuess(this.dialogService.fileLocationPath);
  }

  public onConnectionTargetChanged(target: unknown): void {
    this.dialogService.connectionTestMessage = null;
    this.updateConnectionNameGuess(String(target ?? ''));
  }

  public onConnectionNameChanged(): void {
    this.dialogService.connectionNameEdited = true;
  }

  public submitConnectionOnEnter(event: Event): void {
    event.preventDefault();
    this.submitConnection();
  }

  public testServerConnection(): void {
    this.dialogService.connectionError = null;
    this.dialogService.connectionTestMessage = null;

    if (this.dialogService.connectionModalType !== 'connection') {
      return;
    }

    const target = this.normalizeUri(this.dialogService.connectionUri);
    if (!target) {
      this.dialogService.connectionError = 'Please provide a server URI to test.';
      return;
    }

    this.dialogService.isTestingConnection = true;
    this.databaseService.testConnection(target)
      .then(() => {
        this.dialogService.connectionTestMessage = 'Connection test succeeded.';
      })
      .catch((err: unknown) => {
        this.dialogService.connectionError = err instanceof Error ? err.message : String(err);
      })
      .finally(() => {
        this.dialogService.isTestingConnection = false;
      });
  }

  public submitConnection(): void {
    this.dialogService.connectionError = null;
    let target = this.dialogService.connectionModalType === 'location'
      ? this.dialogService.fileLocationPath.trim()
      : this.dialogService.connectionUri.trim();

    if (!target) {
      this.dialogService.connectionError = 'Please provide a valid target.';
      return;
    }

    if (this.dialogService.connectionModalType === 'connection') {
      target = this.normalizeUri(target);
    }

    this.dialogService.isConnecting = true;
    const displayName = this.dialogService.connectionName.trim() || undefined;
    const connectPromise = this.dialogService.connectionModalType === 'location' && this.dialogService.createIfNotExist
      ? this.databaseService.createSourceLocation(target)
        .then(() => this.databaseService.connectSourceLocation(target, displayName))
      : this.databaseService.connectSourceLocation(target, displayName);

    connectPromise
      .then(() => {
        this.dialogService.isConnecting = false;
        this.dialogService.closeConnectionModal();
        this.databaseService.loadSavedConnections();
      })
      .catch((err: unknown) => {
        this.dialogService.isConnecting = false;
        this.dialogService.connectionError = err instanceof Error ? err.message : String(err);
      });
  }

  private normalizeUri(uri: string): string {
    let trimmed = uri.trim();
    if (trimmed.startsWith('wardrobe://')) {
      const hostPart = trimmed.substring(11);
      if (!hostPart.includes(':')) {
        trimmed = `${trimmed}:24842`;
      }
      return trimmed;
    }

    if (!trimmed.includes('://')) {
      trimmed = trimmed.includes(':') ? `wardrobe://${trimmed}` : `wardrobe://${trimmed}:24842`;
    }
    return trimmed;
  }

  private updateConnectionNameGuess(target: string): void {
    if (this.dialogService.connectionNameEdited) {
      return;
    }

    this.dialogService.connectionName = this.guessConnectionName(target);
  }

  private guessConnectionName(target: string): string {
    const trimmed = target.trim();
    if (!trimmed) {
      return '';
    }

    if (this.dialogService.connectionModalType === 'connection') {
      const normalized = this.normalizeUri(trimmed);
      try {
        const url = new URL(normalized);
        return url.port ? `${url.hostname}:${url.port}` : url.hostname;
      } catch {
        return normalized.replace(/^wardrobe:\/\//, '');
      }
    }

    const normalizedPath = trimmed.replace(/[\\/]+$/, '');
    const parts = normalizedPath.split(/[\\/]+/);
    return parts[parts.length - 1] || normalizedPath;
  }
}
