import { Service, signal, WritableSignal } from '@angular/core';

export interface DialogAction {
    label: string;
    class?: string;
    onClick: () => void;
}

export interface DialogOptions {
    title: string;
    body: string;
    version?: string;
    showInput?: boolean;
    inputPlaceholder?: string;
    inputValue?: string;
    confirmText?: string;
    cancelText?: string;
    onConfirm?: (inputValue?: string) => void;
    actions?: DialogAction[];
}

export type ConnectionModalType = 'location' | 'connection';

@Service()
export class DialogService {
    readonly showDialog: WritableSignal<boolean> = signal(false);
    readonly dialogs: WritableSignal<DialogOptions | null> = signal(null);
    readonly showConnectionModal: WritableSignal<boolean> = signal(false);
    public currentInputValue = '';
    public connectionModalType: ConnectionModalType = 'location';
    public fileLocationPath = '';
    public createIfNotExist = false;
    public connectionUri = '';
    public connectionName = '';
    public connectionNameEdited = false;
    public connectionError: string | null = null;
    public connectionTestMessage: string | null = null;
    public isConnecting = false;
    public isTestingConnection = false;

    openDialog(dialog: DialogOptions): void {
        this.showDialog.set(true);
        this.dialogs.set(dialog);
        this.currentInputValue = dialog.inputValue || '';
    }

    closeDialog(): void {
        this.showDialog.set(false);
        this.dialogs.set(null);
    }

    openConnectionModal(type: ConnectionModalType): void {
        this.connectionModalType = type;
        this.showConnectionModal.set(true);
        this.connectionError = null;
        this.connectionTestMessage = null;
        this.isConnecting = false;
        this.isTestingConnection = false;
        this.connectionName = '';
        this.connectionNameEdited = false;

        if (type === 'location') {
            this.fileLocationPath = '';
            this.createIfNotExist = false;
        } else {
            this.connectionUri = '';
        }
    }

    closeConnectionModal(): void {
        this.showConnectionModal.set(false);
        this.connectionError = null;
        this.connectionTestMessage = null;
        this.isConnecting = false;
        this.isTestingConnection = false;
    }

    confirm(): void {
        const callback = this.dialogs()?.onConfirm;
        if (callback) {
            callback(this.currentInputValue);
        }
        this.closeDialog();
    }

    runAction(action: DialogAction): void {
        action.onClick();
        this.closeDialog();
    }
}
