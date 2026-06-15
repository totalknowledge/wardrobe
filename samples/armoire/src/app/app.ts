import {
  AfterViewInit,
  Component,
  ElementRef,
  viewChild
} from '@angular/core';
import { RouterOutlet } from '@angular/router';
import { WardrobeService } from './wardrobe/wardrobe-service';
import { DialogService } from './dialog/dialog-service';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet],
  templateUrl: './app.html',
  styleUrl: './app.scss'
})
export class App implements AfterViewInit {
  private connectionMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('connectionMenu');
  private databaseMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('databaseMenu');
  private recordsMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('recordsMenu');
  private settingsMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('settingsMenu');
  private aboutMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('helpMenu');

  public openTracker!: Record<string, HTMLDetailsElement>;

  constructor(public databaseService: WardrobeService, public dialogService: DialogService) {}

  public ngAfterViewInit(): void {
    this.openTracker = {
      connection: this.connectionMenu().nativeElement,
      database: this.databaseMenu().nativeElement,
      records: this.recordsMenu().nativeElement,
      settings: this.settingsMenu().nativeElement,
      help: this.aboutMenu().nativeElement
    };
  }

  public about(): void {
    this.clearOpenMenus();
    this.dialogService.openDialog({
      title: 'About Armoire',
      body: 'Armoire is a professional database management for use with the Wardrobe database engine. It provides a high-performance environment for schema architecture, data lifecycle control, and relationship management.',
      version: 'Version 0.1.0',
    });
  }

  public clearOpenMenus(): void {
    this.openTracker['connection']?.removeAttribute('open');
    this.openTracker['database']?.removeAttribute('open');
    this.openTracker['records']?.removeAttribute('open');
    this.openTracker['settings']?.removeAttribute('open');
    this.openTracker['help']?.removeAttribute('open');
  }

  public testWardrobeDatabaseAccess(): void {
    console.log('Testing wardrobe database access command...');
    this.databaseService.testDatabaseAccess('./wardrobe');
    console.log('Wardrobe database access command completed.');
  }
}
