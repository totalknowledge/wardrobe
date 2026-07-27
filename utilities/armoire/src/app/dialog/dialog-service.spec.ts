import { TestBed } from '@angular/core/testing';

import { DialogService } from './dialog-service';

describe('DialogService', () => {
  let service: DialogService;

  beforeEach(() => {
    TestBed.configureTestingModule({});
    service = TestBed.inject(DialogService);
  });

  it('should be created', () => {
    expect(service).toBeTruthy();
  });

  it('opens and closes dialogs with their initial input', () => {
    service.openDialog({
      title: 'Rename',
      body: 'Choose a name',
      inputValue: 'Original',
    });

    expect(service.showDialog()).toBe(true);
    expect(service.dialogs()?.title).toBe('Rename');
    expect(service.currentInputValue).toBe('Original');

    service.closeDialog();
    expect(service.showDialog()).toBe(false);
    expect(service.dialogs()).toBeNull();
  });

  it('resets location connection state when opening and closing the modal', () => {
    service.connectionError = 'old error';
    service.connectionTestMessage = 'old message';
    service.isConnecting = true;
    service.isTestingConnection = true;
    service.connectionName = 'Old';
    service.connectionNameEdited = true;
    service.fileLocationPath = '/old/path';
    service.createIfNotExist = true;

    service.openConnectionModal('location');

    expect(service.showConnectionModal()).toBe(true);
    expect(service.connectionModalType).toBe('location');
    expect(service.fileLocationPath).toBe('');
    expect(service.createIfNotExist).toBe(false);
    expect(service.connectionName).toBe('');
    expect(service.connectionNameEdited).toBe(false);
    expect(service.connectionError).toBeNull();
    expect(service.connectionTestMessage).toBeNull();
    expect(service.isConnecting).toBe(false);
    expect(service.isTestingConnection).toBe(false);

    service.connectionError = 'failed';
    service.connectionTestMessage = 'testing';
    service.isConnecting = true;
    service.isTestingConnection = true;
    service.closeConnectionModal();

    expect(service.showConnectionModal()).toBe(false);
    expect(service.connectionError).toBeNull();
    expect(service.connectionTestMessage).toBeNull();
    expect(service.isConnecting).toBe(false);
    expect(service.isTestingConnection).toBe(false);
  });

  it('resets the URI when opening a server connection modal', () => {
    service.connectionUri = 'wardrobe://old';

    service.openConnectionModal('connection');

    expect(service.connectionModalType).toBe('connection');
    expect(service.connectionUri).toBe('');
  });

  it('confirms dialog input and closes the dialog', () => {
    const onConfirm = vi.fn();
    service.openDialog({
      title: 'Create',
      body: 'Create an item',
      onConfirm,
    });
    service.currentInputValue = 'shirts';

    service.confirm();

    expect(onConfirm).toHaveBeenCalledWith('shirts');
    expect(service.showDialog()).toBe(false);
  });

  it('closes a dialog when no confirmation callback exists', () => {
    service.openDialog({ title: 'Info', body: 'Message' });

    service.confirm();

    expect(service.showDialog()).toBe(false);
  });

  it('runs an action and closes the dialog', () => {
    const onClick = vi.fn();
    service.openDialog({ title: 'Actions', body: 'Choose' });

    service.runAction({ label: 'Run', onClick });

    expect(onClick).toHaveBeenCalledOnce();
    expect(service.showDialog()).toBe(false);
  });
});
