import { ComponentFixture, TestBed } from '@angular/core/testing';

import { ConnectionTreeComponent } from './connection-tree';

describe('ConnectionTree', () => {
  let component: ConnectionTreeComponent;
  let fixture: ComponentFixture<ConnectionTreeComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [ConnectionTreeComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(ConnectionTreeComponent);
    component = fixture.componentInstance;
    await fixture.whenStable();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });
});
