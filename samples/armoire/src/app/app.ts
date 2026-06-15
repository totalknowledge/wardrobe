import {
  AfterViewInit,
  Component,
  ElementRef,
  viewChild
} from '@angular/core';
import { RouterOutlet } from '@angular/router';
import { WardrobeService } from './wardrobe/wardrobe-service';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet],
  templateUrl: './app.html',
  styleUrl: './app.scss'
})
export class App implements AfterViewInit {
  private connectionMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('connectionMenu');

  private settingsMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('settingsMenu');

  private aboutMenu = viewChild.required<ElementRef<HTMLDetailsElement>>('helpMenu');

  public openTracker!: Record<string, HTMLDetailsElement>;

  constructor(public databaseService: WardrobeService) {}

  public ngAfterViewInit(): void {
    this.openTracker = {
      connection: this.connectionMenu().nativeElement,
      settings: this.settingsMenu().nativeElement,
      help: this.aboutMenu().nativeElement
    };
  }

  public about(): void {
    this.clearOpenMenus();
    console.log('armoire is a sample application built with Angular and Tauri.');
  }

  public clearOpenMenus(): void {
    console.log(this.openTracker);
    this.openTracker['help']?.removeAttribute('open');
  }

  public testWardrobeDatabaseAccess(): void {
    console.log('Testing wardrobe database access command...');

    this.databaseService.testDatabaseAccess('./wardrobe');

    console.log('Wardrobe database access command completed.');
  }
}
