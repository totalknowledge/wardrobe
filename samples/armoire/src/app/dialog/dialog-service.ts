import { Service, signal, WritableSignal } from '@angular/core';

@Service()
export class DialogService {
    readonly showDialog: WritableSignal<boolean> = signal(false);
    readonly dialogs: WritableSignal<Record<string, string> | null> = signal(null);

    openDialog(dialog: Record<string, string>): void {
        this.showDialog.set(true);
        this.dialogs.set(dialog);
    }

    closeDialog(): void {
        this.showDialog.set(false);
        this.dialogs.set(null);
    }
}
