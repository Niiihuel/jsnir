#include <SDL2/SDL.h>
int open_gl(int width, int height){
    if(SDL_Init(SDL_INIT_VIDEO))return 0;
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION,3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION,3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK,SDL_GL_CONTEXT_PROFILE_CORE);
    SDL_Window *w=SDL_CreateWindow("JSNIR",0,0,width,height,SDL_WINDOW_OPENGL|SDL_WINDOW_HIDDEN);
    return w && SDL_GL_CreateContext(w);
}
