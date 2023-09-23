; tiny97.asm, copyright Alexander Sotirov

BITS 32
;
; MZ header
; The only two fields that matter are e_magic and e_lfanew

mzhdr:
    dw "MZ"       ; e_magic
    dw 0          ; e_cblp UNUSED

; PE signature
pesig:
    dd "PE"       ; e_cp, e_crlc UNUSED       ; PE signature

; PE header
pehdr:
    dw 0x014C     ; e_cparhdr UNUSED          ; Machine (Intel 386)
    dw 1          ; e_minalloc UNUSED         ; NumberOfSections

;   dd 0xC3582A6A ; e_maxalloc, e_ss UNUSED   ; TimeDateStamp UNUSED

; Entry point
start:
    push byte 42
    pop eax
    ret

codesize equ $ - start

    dd 0          ; e_sp, e_csum UNUSED       ; PointerToSymbolTable UNUSED
    dd 0          ; e_ip, e_cs UNUSED         ; NumberOfSymbols UNUSED
    dw sections-opthdr ; e_lsarlc UNUSED      ; SizeOfOptionalHeader
    dw 0x103      ; e_ovno UNUSED             ; Characteristics

; PE optional header
; The debug directory size at offset 0x94 from here must be 0

filealign equ 4
sect_align equ 4  ; must be 4 because of e_lfanew

%define round(n, r) (((n+(r-1))/r)*r)

opthdr:
    dw 0x10B      ; e_res UNUSED              ; Magic (PE32)
    db 8                                      ; MajorLinkerVersion UNUSED
    db 0                                      ; MinorLinkerVersion UNUSED

; PE code section
sections:
    dd round(codesize, filealign)  ; SizeOfCode UNUSED  ; Name UNUSED
    dd 0  ; e_oemid, e_oeminfo UNUSED ; SizeOfInitializedData UNUSED
    dd codesize  ; e_res2 UNUSED  ; SizeOfUninitializedData UNUSED  ; VirtualSize
    dd start  ; AddressOfEntryPoint  ; VirtualAddress
    dd codesize  ; BaseOfCode UNUSED  ; SizeOfRawData
    dd start  ; BaseOfData UNUSED  ; PointerToRawData
    dd 0x400000  ; ImageBase  ; PointerToRelocations UNUSED
    dd sect_align ; e_lfanew  ; SectionAlignment  ; PointerToLinenumbers UNUSED
    dd filealign  ; FileAlignment  ; NumberOfRelocations, NumberOfLinenumbers UNUSED
    dw 4  ; MajorOperatingSystemVersion UNUSED ; Characteristics UNUSED
    dw 0  ; MinorOperatingSystemVersion UNUSED
    dw 0  ; MajorImageVersion UNUSED
    dw 0  ; MinorImageVersion UNUSED
    dw 4  ; MajorSubsystemVersion
    dw 0  ; MinorSubsystemVersion UNUSED
    dd 0  ; Win32VersionValue UNUSED
    dd round(hdrsize, sect_align)+round(codesize,sect_align) ; SizeOfImage
    dd round(hdrsize, filealign)  ; SizeOfHeaders
    dd 0  ; CheckSum UNUSED
    db 2  ; Subsystem (Win32 GUI)

hdrsize equ $ - $$
filesize equ $ - $$

